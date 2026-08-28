//! Security-тесты: JWT аутентификация.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use shorty_server::{
    AppState, Config,
    auth::{AuthConfig, Claims, create_token, load_keys},
    build_router,
};
use storage::{Cache, InMemoryRepo, telemetry::init_metrics};
use tokio_util::sync::CancellationToken;

fn test_state() -> (AppState, Arc<InMemoryRepo>) {
    let repo = Arc::new(InMemoryRepo::new());
    let stats_storage = Arc::new(domain::stats::StatsStorage::new());
    let mut config = Config::default();
    config.rate_limit_capacity = 10000;
    config.rate_limit_period_secs = 60;

    let auth_config = AuthConfig::default();
    let shutdown_token = CancellationToken::new();

    let state = AppState {
        repo: repo.clone(),
        stats_storage: stats_storage.clone(),
        config: Arc::new(config),
        cache: Cache::disabled(),
        metrics_handle: init_metrics(),
        auth: Some(Arc::new(auth_config)),
        shutdown_token,
        db_pool: None,
    };

    (state, repo)
}

fn test_app() -> Router {
    let (state, _) = test_state();
    build_router(state)
}

/// Получить валидный токен
async fn get_valid_token() -> String {
    let app = test_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": "admin"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    body["access_token"].as_str().unwrap().to_string()
}

/// Создать истёкший токен
fn expired_token() -> String {
    let config = AuthConfig::default();
    let (encoding_key, _) = load_keys(&config).unwrap();

    let mut claims = Claims::new(
        "admin".to_string(),
        "admin".to_string(),
        config.issuer.clone(),
        config.audience.clone(),
        900,
    );
    claims.exp = (chrono::Utc::now() - chrono::Duration::seconds(120)).timestamp() as usize;

    create_token(&claims, encoding_key).unwrap()
}

/// Создать токен с неправильным audience
fn wrong_aud_token() -> String {
    let config = AuthConfig::default();
    let (encoding_key, _) = load_keys(&config).unwrap();

    let claims = Claims::new(
        "admin".to_string(),
        "admin".to_string(),
        config.issuer.clone(),
        "wrong-audience".to_string(),
        900,
    );

    create_token(&claims, encoding_key).unwrap()
}

/// Создать токен, подписанный другим ключом (неверная подпись)
fn wrong_signature_token() -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use std::time::{SystemTime, UNIX_EPOCH};

    let claims = Claims {
        sub: "admin".to_string(),
        exp: (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 300) as usize,
        iat: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize,
        iss: "shorty-service".to_string(),
        aud: "shorty-api".to_string(),
        role: "admin".to_string(),
    };

    let wrong_key = EncodingKey::from_secret(b"wrong-secret-key-for-testing-only");
    jsonwebtoken::encode::<Claims>(&Header::new(Algorithm::HS256), &claims, &wrong_key).unwrap()
}

async fn call_protected_endpoint(app: &Router, token: Option<&str>) -> StatusCode {
    let mut req = Request::builder().method("GET").uri("/api/v1/links");

    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();

    response.status()
}

#[tokio::test]
async fn no_token_returns_401() {
    let app = test_app();
    let status = call_protected_endpoint(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn valid_token_returns_200() {
    let app = test_app();
    let token = get_valid_token().await;
    let status = call_protected_endpoint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn expired_token_returns_401() {
    let app = test_app();
    let token = expired_token();
    let status = call_protected_endpoint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_audience_token_returns_401() {
    let app = test_app();
    let token = wrong_aud_token();
    let status = call_protected_endpoint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_signature_token_returns_401() {
    let app = test_app();
    let token = wrong_signature_token();
    let status = call_protected_endpoint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_endpoint_returns_token() {
    let _app = test_app();
    let token = get_valid_token().await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn login_with_wrong_password_returns_401() {
    let app = test_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": "wrong"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn swagger_ui_is_public() {
    let app = test_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/swagger-ui")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_redirection() || response.status() == StatusCode::OK);
}

#[tokio::test]
async fn openapi_json_is_public() {
    let app = test_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn protected_endpoint_without_auth_returns_unified_error_body() {
    let app = test_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/links")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["code"], "auth_error");
    assert_eq!(body["message"], "missing token");
    assert!(body["request_id"].is_string() || body["request_id"].is_null());
}
