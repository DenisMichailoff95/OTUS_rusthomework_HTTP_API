//! Middleware для rate limiting.
//!
//! Ограничивает количество запросов от одного клиента.
//! Использует алгоритм Token Bucket для плавного восстановления лимита.
//! Клиент идентифицируется по IP-адресу или заголовку X-Api-Key.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use domain::rate_limit::RateLimitConfig;
use storage::rate_limit_storage::RateLimitStorage;

use super::error::AppError;

/// Состояние rate limiter'а для использования в middleware.
///
/// Хранит разделяемое хранилище состояний клиентов.
#[derive(Clone)]
pub struct RateLimitState {
    storage: Arc<RateLimitStorage>,
}

impl RateLimitState {
    /// Создает новый rate limiter с заданными параметрами.
    ///
    /// # Параметры
    /// - `capacity` - максимальное количество запросов за период
    /// - `period_secs` - период восстановления в секундах
    /// - `cleanup_ttl_secs` - TTL для неактивных клиентов
    pub fn new(capacity: u64, period_secs: u64, cleanup_ttl_secs: u64) -> Self {
        let config = RateLimitConfig::new(capacity, period_secs);
        let storage =
            RateLimitStorage::new(config, std::time::Duration::from_secs(cleanup_ttl_secs));
        Self {
            storage: Arc::new(storage),
        }
    }

    /// Проверяет, не превышен ли лимит для клиента.
    ///
    /// # Возвращает
    /// - `Ok(())` - запрос разрешен
    /// - `Err(AppError::RateLimitExceeded)` - лимит превышен
    pub fn check_limit(&self, client_id: &str) -> Result<(), AppError> {
        let now = std::time::Instant::now();
        let (allowed, retry_after) = self.storage.check_and_consume(client_id, now);

        if allowed {
            Ok(())
        } else {
            Err(AppError::RateLimitExceeded { retry_after })
        }
    }
}

/// Middleware для rate limiting.
///
/// Применяется к POST /api/v1/links для ограничения создания ссылок.
pub async fn rate_limit_middleware(
    State(state): State<RateLimitState>,
    req: Request,
    next: Next,
) -> Response {
    // Определяем клиента по IP или X-Api-Key
    let client_id = get_client_id(&req);

    // Проверяем лимит
    if let Err(err) = state.check_limit(&client_id) {
        return err.into_response();
    }

    next.run(req).await
}

/// Получить идентификатор клиента.
///
/// Приоритет определения:
/// 1. X-Api-Key (для API ключей)
/// 2. X-Forwarded-For (для прокси/CDN)
/// 3. Remote-Addr (прямой доступ)
/// 4. "unknown" (fallback)
fn get_client_id(req: &Request) -> String {
    // Пробуем X-Api-Key - предпочтительный способ
    if let Some(api_key) = req.headers().get("x-api-key")
        && let Ok(key) = api_key.to_str()
    {
        return format!("api-key:{}", key);
    }

    // Пробуем X-Forwarded-For для прокси/CDN
    if let Some(forwarded) = req.headers().get("x-forwarded-for")
        && let Ok(ips) = forwarded.to_str()
        && let Some(ip) = ips.split(',').next()
    {
        return format!("ip:{}", ip.trim());
    }

    // Пробуем Remote-Addr для прямых подключений
    if let Some(addr) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        return format!("ip:{}", addr.ip());
    }

    // Fallback - если ничего не подошло
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request, StatusCode};
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_rate_limit_middleware() {
        let rate_limit_state = RateLimitState::new(3, 60, 120);
        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                rate_limit_state.clone(),
                rate_limit_middleware,
            ))
            .with_state(rate_limit_state);

        let client_id = "test-client";
        let req = |id: &str| {
            Request::builder()
                .uri("/test")
                .header("x-api-key", id)
                .body(axum::body::Body::empty())
                .unwrap()
        };

        // Первые 3 запроса должны пройти
        for i in 0..3 {
            let resp = app.clone().oneshot(req(client_id)).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "Request {} should pass",
                i + 1
            );
        }

        // 4-й запрос должен быть отклонён
        let resp = app.clone().oneshot(req(client_id)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // Другой клиент должен пройти
        let resp = app.oneshot(req("other-client")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
