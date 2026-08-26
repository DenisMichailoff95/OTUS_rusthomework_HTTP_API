//! PostgreSQL реализация репозитория ссылок.

use std::time::SystemTime;

use async_trait::async_trait;
use domain::{LinkRepository, LinkStats, RepoError, ShortLink};
use sqlx::{PgPool, Postgres, Transaction};

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
        let row = sqlx::query_as!(
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
                let link = row.into();
                Ok(LinkStats {
                    link,
                    hits: row.hits as u64,
                })
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
            // SQLSTATE 23505 - unique violation
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

        sqlx::query!(
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
        let row = sqlx::query_as!(
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
            Some(row) => Ok(row.into()),
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

    async fn purge_expired(&self, now: SystemTime) -> usize {
        let now = chrono::DateTime::<chrono::Utc>::from(now);
        let result = sqlx::query!(
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;
    use uuid::Uuid;

    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/shorty_test".into());
        PgPool::connect(&url).await.unwrap()
    }

    async fn setup_test_table(pool: &PgPool) {
        sqlx::query!(
            r#"
            CREATE TABLE IF NOT EXISTS links (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                code TEXT UNIQUE NOT NULL,
                target_url TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                expires_at TIMESTAMPTZ,
                version BIGINT NOT NULL DEFAULT 1,
                hits BIGINT NOT NULL DEFAULT 0
            )
            "#
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn test_postgres_repo_crud() {
        let pool = test_pool().await;
        setup_test_table(&pool).await;
        let repo = PostgresRepo::new(pool);

        let link = ShortLink::new("test123", "https://example.com");

        repo.insert(link.clone()).await.unwrap();

        let fetched = repo.get("test123").await.unwrap();
        assert_eq!(fetched.code, "test123");
        assert_eq!(fetched.target_url, "https://example.com");

        let hits = repo.record_hit("test123").await.unwrap();
        assert_eq!(hits, 1);

        let stats = repo.stats("test123").await.unwrap();
        assert_eq!(stats.hits, 1);

        repo.remove("test123").await.unwrap();
        assert!(repo.get("test123").await.is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn test_postgres_repo_purge_expired() {
        let pool = test_pool().await;
        setup_test_table(&pool).await;
        let repo = PostgresRepo::new(pool);
        let now = SystemTime::now();

        let expired = ShortLink::new("expired", "https://example.com/expired")
            .with_expires_at(SystemTime::now());
        repo.insert(expired).await.unwrap();

        let fresh = ShortLink::new("fresh", "https://example.com/fresh");
        repo.insert(fresh).await.unwrap();

        let count = repo.purge_expired(now).await;
        assert_eq!(count, 1);

        assert!(repo.get("expired").await.is_err());
        assert!(repo.get("fresh").await.is_ok());
    }
}
