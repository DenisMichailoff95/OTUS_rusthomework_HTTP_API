//! Redis кеш для горячих данных.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use redis::{AsyncCommands, aio::ConnectionManager};
use serde::{Serialize, de::DeserializeOwned};
use tokio::time::timeout;

/// Конфигурация кеша
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub ttl_secs: u64,
    pub jitter_secs: u64,
    pub op_timeout_ms: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 60,
            jitter_secs: 10,
            op_timeout_ms: 300,
        }
    }
}

/// Счетчики кеша
#[derive(Default)]
pub struct CacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CacheStats {
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        metrics::counter!("cache_hits_total").increment(1);
    }

    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        metrics::counter!("cache_misses_total").increment(1);
    }
}

/// Клиент кеша
#[derive(Clone)]
pub struct Cache {
    conn: Option<ConnectionManager>,
    config: CacheConfig,
    stats: Arc<CacheStats>,
}

impl Cache {
    /// Создание кеша с подключением к Redis
    pub async fn connect(redis_url: &str, config: CacheConfig) -> Self {
        match Self::connect_manager(redis_url).await {
            Some(conn) => Self::from_manager(conn, config),
            None => Self::disabled(),
        }
    }

    async fn connect_manager(redis_url: &str) -> Option<ConnectionManager> {
        let client = redis::Client::open(redis_url).ok()?;
        ConnectionManager::new(client).await.ok()
    }

    pub fn from_manager(conn: ConnectionManager, config: CacheConfig) -> Self {
        Self {
            conn: Some(conn),
            config,
            stats: Arc::default(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            conn: None,
            config: CacheConfig::default(),
            stats: Arc::default(),
        }
    }

    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Получение значения из кеша
    #[tracing::instrument(name = "cache.get", level = "debug", skip(self))]
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let Some(mut conn) = self.conn.clone() else {
            self.stats.record_miss();
            return None;
        };

        let result = timeout(
            Duration::from_millis(self.config.op_timeout_ms),
            conn.get::<_, Option<String>>(key),
        )
        .await;

        match result {
            Ok(Ok(Some(raw))) => match serde_json::from_str(&raw) {
                Ok(value) => {
                    self.stats.record_hit();
                    Some(value)
                }
                Err(err) => {
                    tracing::warn!(key, %err, "failed to deserialize cached value");
                    self.stats.record_miss();
                    None
                }
            },
            Ok(Ok(None)) => {
                self.stats.record_miss();
                None
            }
            Ok(Err(err)) => {
                tracing::warn!(key, %err, "redis GET failed");
                self.stats.record_miss();
                None
            }
            Err(_) => {
                tracing::warn!(key, "redis GET timed out");
                self.stats.record_miss();
                None
            }
        }
    }

    /// Запись значения в кеш с TTL
    #[tracing::instrument(name = "cache.set", level = "debug", skip(self, value))]
    pub async fn set<T: Serialize>(&self, key: &str, value: &T) {
        let Some(mut conn) = self.conn.clone() else {
            return;
        };

        let raw = match serde_json::to_string(value) {
            Ok(raw) => raw,
            Err(err) => {
                tracing::warn!(key, %err, "failed to serialize value");
                return;
            }
        };

        let ttl = self.jittered_ttl();
        let result = timeout(
            Duration::from_millis(self.config.op_timeout_ms),
            conn.set_ex::<_, _, ()>(key, raw, ttl.as_secs()),
        )
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!(key, %err, "redis SET failed"),
            Err(_) => tracing::warn!(key, "redis SET timed out"),
        }
    }

    /// Инвалидация ключа
    #[tracing::instrument(name = "cache.invalidate", level = "debug", skip(self))]
    pub async fn invalidate(&self, key: &str) {
        let Some(mut conn) = self.conn.clone() else {
            return;
        };

        let result = timeout(
            Duration::from_millis(self.config.op_timeout_ms),
            conn.del::<_, ()>(key),
        )
        .await;

        match result {
            Ok(Ok(())) => tracing::debug!(key, "cache invalidated"),
            Ok(Err(err)) => tracing::warn!(key, %err, "redis DEL failed"),
            Err(_) => tracing::warn!(key, "redis DEL timed out"),
        }
    }

    /// TTL с jitter
    fn jittered_ttl(&self) -> Duration {
        use rand::Rng;
        let base = self.config.ttl_secs as i64;
        let jitter = self.config.jitter_secs as i64;
        let offset = rand::thread_rng().gen_range(-jitter..=jitter);
        Duration::from_secs((base + offset).max(1) as u64)
    }
}

/// Формирование ключа для ссылки
pub fn link_key(code: &str) -> String {
    format!("shorty:v1:link:{}", code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_key_format() {
        let key = link_key("abc123");
        assert_eq!(key, "shorty:v1:link:abc123");
    }

    #[test]
    fn test_jittered_ttl_stays_in_bounds() {
        let config = CacheConfig::default();
        let cache = Cache::disabled();

        for _ in 0..100 {
            let ttl = cache.jittered_ttl();
            let secs = ttl.as_secs();
            assert!((50..=70).contains(&secs), "ttl = {}", secs);
        }
    }
}
