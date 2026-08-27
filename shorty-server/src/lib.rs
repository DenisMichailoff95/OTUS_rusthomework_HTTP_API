//! Сервис коротких ссылок `shorty`.

pub mod api;
pub mod auth;
pub mod cleanup;
pub mod config;
pub mod grpc;
pub mod request_id;

use std::sync::Arc;

use axum::http::HeaderValue;
use axum::{
    Router,
    routing::{get, post},
};
use domain::LinkRepository;
use metrics_exporter_prometheus::PrometheusHandle;
use storage::Cache;
use tower::ServiceBuilder;
use tower_http::{
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use utoipa::{Modify, OpenApi};
use utoipa_swagger_ui::SwaggerUi;

pub use config::Config;

use crate::api::error::ErrorBody;
use crate::api::handlers::{
    create_link, delete_link, fallback_404, get_link, healthz, list_links, redirect, slow,
    slow_blocking, update_link, version,
};
use crate::api::rate_limit::RateLimitState;
use crate::api::stats::{get_link_stats, get_top_stats};
use crate::auth::{AuthConfig, auth_middleware, login_handler};

/// Состояние приложения.
#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn LinkRepository>,
    pub stats_storage: Arc<domain::stats::StatsStorage>,
    pub config: Arc<Config>,
    pub cache: Cache,
    pub metrics_handle: PrometheusHandle,
    pub auth: Option<Arc<AuthConfig>>,
}

/// OpenAPI спецификация
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Shorty API",
        version = "1.0.0",
        description = "Сервис сокращения ссылок с JWT-аутентификацией"
    ),
    components(
        schemas(
            ErrorBody,
            crate::api::dto::LinkResponse,
            crate::api::dto::CreateLinkRequest,
            crate::api::dto::UpdateLinkRequest,
            crate::api::dto::ListLinksResponse,
            crate::api::stats::LinkStatsResponse,
            crate::api::stats::TopStatsResponse,
            crate::api::stats::TopLink,
            crate::auth::LoginRequest,
            crate::auth::LoginResponse,
            crate::api::handlers::Health,
            crate::api::handlers::Version,
        )
    ),
    tags(
        (name = "shorty", description = "Операции с короткими ссылками"),
        (name = "auth", description = "Аутентификация"),
        (name = "health", description = "Health check")
    ),
    security(
        ("bearerAuth" = [])
    ),
    modifiers(&BearerAuthAddon)
)]
pub struct ApiDoc;

struct BearerAuthAddon;

impl Modify for BearerAuthAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.components = Some(
            utoipa::openapi::schema::ComponentsBuilder::new()
                .security_scheme(
                    "bearerAuth",
                    utoipa::openapi::security::SecurityScheme::Http(
                        utoipa::openapi::security::HttpBuilder::new()
                            .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                            .build(),
                    ),
                )
                .build(),
        );
    }
}

/// Сборка приложения.
pub fn build_router(state: AppState) -> Router {
    let _rate_limit_state = RateLimitState::new(
        state.config.rate_limit_capacity,
        state.config.rate_limit_period_secs,
        state.config.rate_limit_cleanup_ttl_secs,
    );

    let metrics_handle = state.metrics_handle.clone();

    // Публичные routes (не требуют аутентификации)
    let public_routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        .route("/auth/login", post(login_handler))
        .route("/{code}", get(redirect))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));

    // Защищённые API routes (требуют JWT)
    let protected_routes = Router::new()
        .route("/links", post(create_link).get(list_links))
        .route(
            "/links/{code}",
            get(get_link).put(update_link).delete(delete_link),
        )
        .route("/links/{code}/stats", get(get_link_stats))
        .route("/stats/top", get(get_top_stats))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // Основной роутер
    Router::new()
        .merge(public_routes)
        .nest("/api/v1", protected_routes)
        .route("/slow", get(slow))
        .route("/slow-blocking", get(slow_blocking))
        .route(
            "/metrics",
            get(move || async move {
                let payload = metrics_handle.render();
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        HeaderValue::from_static("text/plain; version=0.0.4"),
                    )],
                    payload,
                )
            }),
        )
        .fallback(fallback_404)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(axum::middleware::from_fn(request_id::request_id_scope))
                .layer(TraceLayer::new_for_http().make_span_with(make_span))
                .layer(axum::middleware::from_fn(http_metrics_middleware))
                .layer(TimeoutLayer::with_status_code(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    state.config.request_timeout,
                ))
                .layer(RequestBodyLimitLayer::new(state.config.max_body_bytes))
                .layer(PropagateRequestIdLayer::x_request_id()),
        )
        .with_state(state)
}

fn make_span(req: &axum::extract::Request) -> tracing::Span {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    tracing::info_span!(
        "http_request",
        method = %req.method(),
        uri = %req.uri(),
        request_id = %request_id,
    )
}

#[tracing::instrument(skip_all)]
async fn http_metrics_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let route = route_template(req.uri().path());
    let start = std::time::Instant::now();

    let response = next.run(req).await;

    let latency = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    metrics::counter!("http_requests_total", "method" => method.to_string(), "route" => route.clone(), "status" => status).increment(1);
    metrics::histogram!("http_request_duration_seconds", "method" => method.to_string(), "route" => route).record(latency);

    response
}

fn route_template(path: &str) -> String {
    if path.starts_with("/api/v1/links/") && path.len() > 14 {
        if path.matches('/').count() == 3 {
            return "/api/v1/links/{code}".to_string();
        }
        if path == "/api/v1/links" || path == "/api/v1/links/" {
            return "/api/v1/links".to_string();
        }
    }
    if path == "/healthz" {
        return "/healthz".to_string();
    }
    if path == "/version" {
        return "/version".to_string();
    }
    if path == "/slow" {
        return "/slow".to_string();
    }
    if path == "/slow-blocking" {
        return "/slow-blocking".to_string();
    }
    if path == "/stats/top" {
        return "/stats/top".to_string();
    }
    if path == "/metrics" {
        return "/metrics".to_string();
    }
    if path.starts_with("/api/v1/links/") && path.ends_with("/stats") {
        return "/api/v1/links/{code}/stats".to_string();
    }
    if path.len() > 1 && !path.starts_with('/') {
        return "/{code}".to_string();
    }
    if path.len() > 1
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return "/{code}".to_string();
    }
    path.to_string()
}
