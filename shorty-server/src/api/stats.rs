//! Эндпоинты статистики.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{AppError, ErrorBody};
use crate::AppState;

/// Запрос для топа ссылок.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TopQuery {
    /// Лимит топа (макс 100)
    #[schema(example = 10)]
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

impl TopQuery {
    pub fn limit(&self) -> usize {
        self.limit.min(100)
    }
}

/// Ответ со статистикой.
#[derive(Debug, Serialize, ToSchema)]
pub struct LinkStatsResponse {
    /// Код ссылки
    pub code: String,
    /// Общее количество переходов
    pub total_hits: u64,
    /// Переходы за последние 60 секунд
    pub hits_last_60s: u64,
}

/// Ответ с топом ссылок.
#[derive(Debug, Serialize, ToSchema)]
pub struct TopStatsResponse {
    /// Список топовых ссылок
    pub links: Vec<TopLink>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TopLink {
    /// Код ссылки
    pub code: String,
    /// Общее количество переходов
    pub total_hits: u64,
}

/// GET /api/v1/links/{code}/stats - статистика для конкретной ссылки.
#[utoipa::path(
    get,
    path = "/api/v1/links/{code}/stats",
    tags = ["shorty"],
    params(
        ("code" = String, Path, description = "Код ссылки")
    ),
    responses(
        (status = 200, description = "Статистика ссылки", body = LinkStatsResponse),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 403, description = "Доступ запрещён", body = ErrorBody),
        (status = 404, description = "Ссылка не найдена", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
pub async fn get_link_stats(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<LinkStatsResponse>, AppError> {
    let link = state.repo.get(&code).await?;
    if link.is_expired(std::time::SystemTime::now()) {
        return Err(AppError::NotFound);
    }

    let stats = state.stats_storage.get(&code);
    let (total_hits, hits_last_60s) = match stats {
        Some(s) => {
            let guard = s
                .read()
                .map_err(|_| AppError::Internal(anyhow::anyhow!("failed to read stats")))?;
            (guard.get_total(), guard.get_last_60s())
        }
        None => (0, 0),
    };

    Ok(Json(LinkStatsResponse {
        code,
        total_hits,
        hits_last_60s,
    }))
}

/// GET /api/v1/stats/top?limit=N - топ ссылок по переходам.
#[utoipa::path(
    get,
    path = "/stats/top",
    tags = ["shorty"],
    params(
        ("limit" = Option<usize>, Query, description = "Лимит топа (макс 100)")
    ),
    responses(
        (status = 200, description = "Топ ссылок", body = TopStatsResponse),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 403, description = "Доступ запрещён", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
pub async fn get_top_stats(
    State(state): State<AppState>,
    Query(query): Query<TopQuery>,
) -> Json<TopStatsResponse> {
    let limit = query.limit();
    let top = state.stats_storage.get_top(limit);

    Json(TopStatsResponse {
        links: top
            .into_iter()
            .map(|(code, total_hits)| TopLink { code, total_hits })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use domain::{LinkRepository, ShortLink};
    use std::sync::Arc;
    use storage::{Cache, InMemoryRepo};

    #[tokio::test]
    async fn test_stats_endpoints() {
        use tokio_util::sync::CancellationToken;
        let repo = Arc::new(InMemoryRepo::new());
        let stats_storage = Arc::new(domain::stats::StatsStorage::new());
        let state = AppState {
            repo: repo.clone(),
            stats_storage: stats_storage.clone(),
            config: Arc::new(crate::Config::default()),
            cache: Cache::disabled(),
            metrics_handle: storage::telemetry::init_metrics(),
            auth: None,
            shutdown_token: CancellationToken::new(),
            db_pool: None,
        };

        // Используем асинхронный метод
        let link = ShortLink::new("test", "https://example.com");
        repo.insert(link).await.unwrap();

        let stats = stats_storage.get_or_create("test");
        stats
            .write()
            .unwrap()
            .record_hit(std::time::SystemTime::now());
        stats
            .write()
            .unwrap()
            .record_hit(std::time::SystemTime::now());

        let response = get_link_stats(State(state.clone()), Path("test".to_string()))
            .await
            .unwrap();

        assert_eq!(response.code, "test");
        assert_eq!(response.total_hits, 2);
        assert_eq!(response.hits_last_60s, 2);

        let top = get_top_stats(State(state), Query(TopQuery { limit: 10 })).await;
        assert_eq!(top.links.len(), 1);
        assert_eq!(top.links[0].code, "test");
        assert_eq!(top.links[0].total_hits, 2);
    }
}
