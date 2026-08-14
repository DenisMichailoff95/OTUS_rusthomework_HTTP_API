//! Сервис коротких ссылок `shorty`.

pub mod api;
pub mod cleanup;
pub mod request_id;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    routing::{get, post},
};
use domain::LinkRepository;
use tower::ServiceBuilder;
use tower_http::{
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::api::rate_limit::{RateLimitState, rate_limit_middleware};

/// Конфигурация приложения.
#[derive(Debug, Clone)]
pub struct Config {
    pub code_length: usize,
    pub max_generate_attempts: usize,
    pub request_timeout: Duration,
    pub max_body_bytes: usize,
    /// Лимит созданий ссылок в минуту.
    pub rate_limit_capacity: u64,
    /// Период rate limiter'а (в секундах).
    pub rate_limit_period_secs: u64,
    /// TTL для неактивных записей rate limiter'а.
    pub rate_limit_cleanup_ttl_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            code_length: 8,
            max_generate_attempts: 5,
            request_timeout: Duration::from_secs(5),
            max_body_bytes: 16 * 1024,
            rate_limit_capacity: 10,
            rate_limit_period_secs: 60,
            rate_limit_cleanup_ttl_secs: 120,
        }
    }
}

/// Состояние приложения.
#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn LinkRepository>,
    pub stats_storage: Arc<domain::stats::StatsStorage>,
    pub config: Arc<Config>,
}

/// Сборка приложения.
pub fn build_router(state: AppState) -> Router {
    let rate_limit_state = RateLimitState::new(
        state.config.rate_limit_capacity,
        state.config.rate_limit_period_secs,
        state.config.rate_limit_cleanup_ttl_secs,
    );

    // API v1 routes с rate limiting
    let api_v1 = Router::new()
        .route("/links", post(api::handlers::create_link))
        .route(
            "/links/{code}",
            get(api::handlers::get_link).delete(api::handlers::delete_link),
        )
        .route("/links/{code}/stats", get(api::stats::get_link_stats))
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
        .nest("/api/v1", api_v1)
        .route("/{code}", get(api::handlers::redirect))
        .fallback(api::handlers::fallback_404)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(axum::middleware::from_fn(request_id::request_id_scope))
                .layer(TraceLayer::new_for_http().make_span_with(make_span))
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
