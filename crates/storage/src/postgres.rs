//! PostgreSQL реализация репозитория ссылок.

use std::time::SystemTime;

use async_trait::async_trait;
use domain::{LinkRepository, LinkStats, RepoError, ShortLink};
use sqlx::PgPool;

/// Строка из таблицы links
#[derive(Debug, sqlx::FromRow)]
struct LinkRow {
    pub id: uuid::Uuid,
    pub code: String,
    pub target_url: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub version: i64,
    pub hits: i64,
}

impl From<LinkRow> for ShortLink {
    fn from(row: LinkRow) -> Self {
        ShortLink {
            id: Some(row.id),
            code: row.code,
            target_url: row.target_url,
            created_at: row.created_at.into(),
            updated_at: row.updated_at.into(),
            expires_at: row.expires_at.map(Into::into),
            version: row.version,
        }
    }
}

/// PostgreSQL репозиторий
#[derive(Clone)]
pub struct PostgresRepo {
    pool: PgPool,
}

impl PostgresRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Получение статистики для ссылки
    async fn get_stats_internal(&self, code: &str) -> Result<LinkStats, RepoError> {
        let row: Option<LinkRow> = sqlx::query_as!(
            LinkRow,
            r#"
            SELECT 
                id, code, target_url, 
                created_at, updated_at, expires_at,
                version, hits
            FROM links 
            WHERE code = $1
            "#,
            code
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sql_error)?;

        match row {
            Some(row) => {
                let hits = row.hits as u64;
                let link: ShortLink = row.into();
                Ok(LinkStats { link, hits })
            }
            None => Err(RepoError::NotFound(code.to_string())),
        }
    }
}

/// Маппинг SQL ошибок в доменные ошибки
fn map_sql_error(err: sqlx::Error) -> RepoError {
    match &err {
        sqlx::Error::RowNotFound => RepoError::NotFound("not found".to_string()),
        sqlx::Error::Database(db_err) => {
            if db_err.code().as_deref() == Some("23505") {
                let msg = db_err.message();
                if msg.contains("code") {
                    RepoError::CodeTaken("code already exists".to_string())
                } else {
                    RepoError::CodeTaken("duplicate value".to_string())
                }
            } else {
                RepoError::Internal(anyhow::anyhow!(err))
            }
        }
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => RepoError::Unavailable,
        _ => RepoError::Internal(anyhow::anyhow!(err)),
    }
}

#[async_trait]
impl LinkRepository for PostgresRepo {
    async fn insert(&self, link: ShortLink) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(map_sql_error)?;

        let expires_at = link.expires_at.map(|t| {
            let duration = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
            chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                .unwrap_or_else(chrono::Utc::now)
        });

        let _result = sqlx::query!(
            r#"
            INSERT INTO links (code, target_url, expires_at, created_at, updated_at)
            VALUES ($1, $2, $3, now(), now())
            "#,
            link.code,
            link.target_url,
            expires_at,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_sql_error)?;

        tx.commit().await.map_err(map_sql_error)?;
        Ok(())
    }

    async fn get(&self, code: &str) -> Result<ShortLink, RepoError> {
        let row: Option<LinkRow> = sqlx::query_as!(
            LinkRow,
            r#"
            SELECT 
                id, code, target_url, 
                created_at, updated_at, expires_at,
                version, hits
            FROM links 
            WHERE code = $1
            "#,
            code
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sql_error)?;

        match row {
            Some(row) => Ok(ShortLink::from(row)),
            None => Err(RepoError::NotFound(code.to_string())),
        }
    }

    async fn remove(&self, code: &str) -> Result<(), RepoError> {
        let result = sqlx::query!("DELETE FROM links WHERE code = $1", code)
            .execute(&self.pool)
            .await
            .map_err(map_sql_error)?;

        if result.rows_affected() == 0 {
            Err(RepoError::NotFound(code.to_string()))
        } else {
            Ok(())
        }
    }

    async fn record_hit(&self, code: &str) -> Result<u64, RepoError> {
        let result = sqlx::query!(
            r#"
            UPDATE links 
            SET hits = hits + 1 
            WHERE code = $1 
            RETURNING hits
            "#,
            code
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sql_error)?;

        match result {
            Some(row) => Ok(row.hits as u64),
            None => Err(RepoError::NotFound(code.to_string())),
        }
    }

    async fn stats(&self, code: &str) -> Result<LinkStats, RepoError> {
        self.get_stats_internal(code).await
    }

    async fn update(
        &self,
        code: &str,
        target_url: &str,
        version: i64,
    ) -> Result<ShortLink, RepoError> {
        let row = sqlx::query_as!(
            LinkRow,
            r#"
            UPDATE links 
            SET target_url = $1, updated_at = now(), version = version + 1
            WHERE code = $2 AND version = $3
            RETURNING id, code, target_url, created_at, updated_at, expires_at, version, hits
            "#,
            target_url,
            code,
            version
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sql_error)?;

        match row {
            Some(row) => Ok(row.into()),
            None => {
                let exists = sqlx::query!("SELECT 1 AS exists FROM links WHERE code = $1", code)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(map_sql_error)?;
                if exists.is_some() {
                    Err(RepoError::VersionConflict)
                } else {
                    Err(RepoError::NotFound(code.to_string()))
                }
            }
        }
    }

    async fn list(
        &self,
        limit: u64,
        cursor: Option<(&str, &str)>,
    ) -> Result<(Vec<ShortLink>, Option<(String, String)>), RepoError> {
        let rows: Vec<LinkRow> = match cursor {
            Some((created_at_str, code)) => {
                let ts = chrono::DateTime::parse_from_rfc3339(created_at_str)
                    .map(|dt| dt.to_utc())
                    .unwrap_or_else(|_| chrono::Utc::now());
                sqlx::query_as!(
                    LinkRow,
                    r#"
                    SELECT id, code, target_url, created_at, updated_at, expires_at, version, hits
                    FROM links
                    WHERE created_at < $1 OR (created_at = $1 AND code < $2)
                    ORDER BY created_at DESC, code DESC
                    LIMIT $3
                    "#,
                    ts,
                    code,
                    limit as i64,
                )
                .fetch_all(&self.pool)
                .await
                .map_err(map_sql_error)?
            }
            None => sqlx::query_as!(
                LinkRow,
                r#"
                    SELECT id, code, target_url, created_at, updated_at, expires_at, version, hits
                    FROM links
                    ORDER BY created_at DESC, code DESC
                    LIMIT $1
                    "#,
                limit as i64,
            )
            .fetch_all(&self.pool)
            .await
            .map_err(map_sql_error)?,
        };

        let links: Vec<ShortLink> = rows.into_iter().map(Into::into).collect();
        let next_cursor = links.last().map(|link| {
            let ts = chrono::DateTime::<chrono::Utc>::from(link.created_at).to_rfc3339();
            (ts, link.code.clone())
        });

        Ok((links, next_cursor))
    }

    async fn purge_expired(&self, now: SystemTime) -> usize {
        let now = chrono::DateTime::<chrono::Utc>::from(now);
        let result: Result<sqlx::postgres::PgQueryResult, sqlx::Error> = sqlx::query!(
            r#"
            DELETE FROM links 
            WHERE expires_at IS NOT NULL AND expires_at <= $1
            "#,
            now
        )
        .execute(&self.pool)
        .await;

        match result {
            Ok(res) => res.rows_affected() as usize,
            Err(err) => {
                tracing::error!(error = %err, "failed to purge expired links");
                0
            }
        }
    }
}
