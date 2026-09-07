//! Сервис коротких ссылок `shorty`.

pub mod api;
pub mod cleanup;
pub mod config;
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

pub use config::Config;

use crate::api::rate_limit::{RateLimitState, rate_limit_middleware};

/// Состояние приложения.
#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn LinkRepository>,
    pub stats_storage: Arc<domain::stats::StatsStorage>,
    pub config: Arc<Config>,
    pub cache: Cache,
    pub metrics_handle: PrometheusHandle,
}

/// Сборка приложения.
pub fn build_router(state: AppState) -> Router {
    let rate_limit_state = RateLimitState::new(
        state.config.rate_limit_capacity,
        state.config.rate_limit_period_secs,
        state.config.rate_limit_cleanup_ttl_secs,
    );

    let metrics_handle = state.metrics_handle.clone();

    // API v1 routes с rate limiting
    let api_v1 = Router::new()
        .route("/links", post(api::handlers::create_link))
        .route(
            "/links/{code}",
            get(api::handlers::get_link)
                .put(api::handlers::update_link)
                .delete(api::handlers::delete_link),
        )
        .route("/links/{code}/stats", get(api::stats::get_link_stats))
        .route("/links", get(api::handlers::list_links))
        .layer(axum::middleware::from_fn_with_state(
            rate_limit_state.clone(),
            rate_limit_middleware,
        ));

    // Основной роутер
    Router::new()
        .route("/healthz", get(api::handlers::healthz))
        .route("/version", get(api::handlers::version))
        .route("/slow", get(api::handlers::slow))
        .route("/slow-blocking", get(api::handlers::slow_blocking))
        .route("/stats/top", get(api::stats::get_top_stats))
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
        .nest("/api/v1", api_v1)
        .route("/{code}", get(api::handlers::redirect))
        .fallback(api::handlers::fallback_404)
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
