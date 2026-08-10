//! Доменная логика rate limiter'а.
//!
//! Реализация алгоритма Token Bucket для ограничения частоты запросов.
//! Поддерживает как скользящее окно с виртуальным временем для тестов.

use std::time::Instant;

/// Состояние одного клиента в rate limiter'е.
#[derive(Debug, Clone)]
pub struct RateLimitState {
    /// Количество доступных токенов (с учётом дробных частей).
    pub tokens: f64,
    /// Время последнего обновления состояния.
    pub last_update: Instant,
}

/// Конфигурация rate limiter'а.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Максимальное количество запросов за период.
    pub capacity: u64,
    /// Период восстановления токенов (в секундах).
    pub period_secs: u64,
}

impl RateLimitConfig {
    pub fn new(capacity: u64, period_secs: u64) -> Self {
        Self { capacity, period_secs }
    }

    /// Скорость пополнения токенов (токенов в секунду).
    fn refill_rate(&self) -> f64 {
        self.capacity as f64 / self.period_secs as f64
    }
}

impl RateLimitState {
    pub fn new_full(capacity: u64) -> Self {
        Self {
            tokens: capacity as f64,
            last_update: Instant::now(),
        }
    }

    /// Проверить, доступен ли запрос, и обновить состояние.
    /// Возвращает (разрешено, новое состояние, время до восстановления).
    ///
    /// Время до восстановления (в секундах) используется для заголовка Retry-After.
    pub fn try_consume(
        &self,
        config: &RateLimitConfig,
        now: Instant,
    ) -> (bool, Self, u64) {
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        let refill = elapsed * config.refill_rate();
        
        let new_tokens = (self.tokens + refill).min(config.capacity as f64);

        if new_tokens >= 1.0 {
            let new_state = Self {
                tokens: new_tokens - 1.0,
                last_update: now,
            };
            (true, new_state, 0)
        } else {
            // Вычисляем время до появления одного токена
            let shortage = 1.0 - new_tokens;
            let retry_after = (shortage / config.refill_rate()).ceil() as u64;
            // Если недостаток очень маленький, но времени прошло мало
            let retry_after = if retry_after == 0 && shortage > 0.0 {
                1
            } else {
                retry_after
            };
            let new_state = Self {
                tokens: new_tokens,
                last_update: now,
            };
            (false, new_state, retry_after)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_rate_limit_consumption() {
        let config = RateLimitConfig::new(10, 60); // 10 запросов в минуту
        let mut state = RateLimitState::new_full(10);
        
        // Первые 10 запросов должны проходить
        for i in 0..10 {
            let (allowed, new_state, retry_after) = 
                state.try_consume(&config, Instant::now());
            assert!(allowed, "Request {} should be allowed", i + 1);
            assert_eq!(retry_after, 0);
            state = new_state;
        }
        
        // 11-й запрос должен быть отклонён
        let (allowed, _, retry_after) = 
            state.try_consume(&config, Instant::now());
        assert!(!allowed);
        assert!(retry_after > 0);
    }

    #[test]
    fn test_rate_limit_refill() {
        let config = RateLimitConfig::new(10, 60);
        let now = Instant::now();
        
        // Создаём пустое состояние
        let state = RateLimitState {
            tokens: 0.0,
            last_update: now,
        };
        
        // Через 6 секунд (10% периода) должно появиться 1 токен
        let later = now + Duration::from_secs(6);
        let (allowed, new_state, _) = state.try_consume(&config, later);
        assert!(allowed);
        // Должен остаться небольшой остаток
        assert!(new_state.tokens < 0.1);
        
        // Ещё через 6 секунд - ещё один токен
        let later2 = later + Duration::from_secs(6);
        let (allowed, new_state, _) = new_state.try_consume(&config, later2);
        assert!(allowed);
        assert!(new_state.tokens < 0.2);
    }

    #[test]
    fn test_retry_after_calculation() {
        let config = RateLimitConfig::new(1, 5); // 1 запрос в 5 секунд
        let now = Instant::now();
        let state = RateLimitState::new_full(1);
        
        // Тратим единственный токен
        let (_, state, _) = state.try_consume(&config, now);
        
        // Сразу после этого должен быть retry_after ~5 секунд
        let (_, _, retry_after) = state.try_consume(&config, now);
        assert!(retry_after >= 5 && retry_after <= 6);
    }
}