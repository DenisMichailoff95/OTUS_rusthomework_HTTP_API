//! Хранение состояния rate limiter'а.
//!
//! Использует DashMap для конкурентного доступа и ленивую очистку
//! устаревших записей.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use ::dashmap::DashMap;
use domain::rate_limit::{RateLimitConfig, RateLimitState};

/// Хранилище состояний rate limiter'а для разных клиентов.
#[derive(Clone)]
pub struct RateLimitStorage {
    inner: Arc<DashMap<String, RateLimitState>>,
    config: RateLimitConfig,
    /// Время жизни неактивных записей (после этого они удаляются лениво).
    ttl: Duration,
}

impl RateLimitStorage {
    pub fn new(config: RateLimitConfig, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            config,
            ttl,
        }
    }

    /// Проверить, разрешён ли запрос для клиента.
    /// Возвращает (разрешено, retry_after_secs).
    pub fn check_and_consume(&self, client_id: &str, now: Instant) -> (bool, u64) {
        // Ленивая очистка старых записей
        self.cleanup_stale(now);

        // Получаем или создаём состояние для клиента
        let mut entry = self
            .inner
            .entry(client_id.to_string())
            .or_insert_with(|| RateLimitState::new_full(self.config.capacity));

        let (allowed, new_state, retry_after) = entry.try_consume(&self.config, now);

        if allowed || retry_after > 0 {
            *entry = new_state;
        }

        (allowed, retry_after)
    }

    /// Лениво удаляет записи, которые не обновлялись дольше TTL.
    fn cleanup_stale(&self, now: Instant) {
        let threshold = now - self.ttl;
        self.inner.retain(|_, state| state.last_update >= threshold);
    }

    /// Получить текущий размер хранилища (для тестов).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Очистить все записи (для тестов).
    pub fn clear(&self) {
        self.inner.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_storage() {
        let config = RateLimitConfig::new(3, 60);
        let storage = RateLimitStorage::new(config, Duration::from_secs(120));
        let client = "test-client";
        let now = Instant::now();

        // Первые 3 запроса должны пройти
        for i in 0..3 {
            let (allowed, retry_after) = storage.check_and_consume(client, now);
            assert!(allowed, "Request {} should be allowed", i + 1);
            assert_eq!(retry_after, 0);
        }

        // 4-й запрос должен быть отклонён
        let (allowed, retry_after) = storage.check_and_consume(client, now);
        assert!(!allowed);
        assert!(retry_after > 0);

        // Проверяем, что запись существует
        assert_eq!(storage.len(), 1);
    }

    #[test]
    fn test_rate_limit_recovery() {
        let config = RateLimitConfig::new(3, 60);
        let storage = RateLimitStorage::new(config, Duration::from_secs(120));
        let client = "test-client";
        let now = Instant::now();

        // Заполняем все токены
        for _ in 0..3 {
            storage.check_and_consume(client, now);
        }

        // Через 20 секунд должен появиться 1 токен
        let later = now + Duration::from_secs(20);
        let (allowed, retry_after) = storage.check_and_consume(client, later);
        assert!(allowed);
        assert_eq!(retry_after, 0);

        // Снова все токены потрачены
        for _ in 0..3 {
            storage.check_and_consume(client, later);
        }

        let (allowed, retry_after) = storage.check_and_consume(client, later);
        assert!(!allowed);
        assert!(retry_after > 0);
    }

    #[test]
    fn test_stale_cleanup() {
        let config = RateLimitConfig::new(10, 60);
        let storage = RateLimitStorage::new(config, Duration::from_secs(10));
        let now = Instant::now();

        // Создаём запись для клиента
        storage.check_and_consume("client1", now);
        assert_eq!(storage.len(), 1);

        // Через 15 секунд - должна быть очищена лениво
        let later = now + Duration::from_secs(15);
        storage.check_and_consume("client2", later);
        assert_eq!(storage.len(), 1); // client1 удалён, client2 добавлен
    }
}
