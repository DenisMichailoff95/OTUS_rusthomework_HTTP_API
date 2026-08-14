//! Эндпоинты статистики.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};

use super::error::AppError;
use crate::AppState;

/// Запрос для топа ссылок.
#[derive(Debug, Deserialize)]
pub struct TopQuery {
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
#[derive(Debug, Serialize)]
pub struct LinkStatsResponse {
    pub code: String,
    pub total_hits: u64,
    pub hits_last_60s: u64,
}

/// Ответ с топом ссылок.
#[derive(Debug, Serialize)]
pub struct TopStatsResponse {
    pub links: Vec<TopLink>,
}

#[derive(Debug, Serialize)]
pub struct TopLink {
    pub code: String,
    pub total_hits: u64,
}

/// GET /api/v1/links/{code}/stats - статистика для конкретной ссылки.
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
    use storage::InMemoryRepo;

    #[tokio::test]
    async fn test_stats_endpoints() {
        let repo = Arc::new(InMemoryRepo::new());
        let stats_storage = Arc::new(domain::stats::StatsStorage::new());
        let state = AppState {
            repo: repo.clone(),
            stats_storage: stats_storage.clone(),
            config: Arc::new(crate::Config::default()),
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
