//! Статистика переходов со скользящим окном.
//!
//! Реализация с использованием кольцевого буфера для хранения
//! количества переходов по каждой секунде. Точность - 1 секунда,
//! окно - 60 секунд.

use std::{
    cmp::Reverse,
    collections::HashMap,
    sync::{Arc, RwLock},
    time::SystemTime,
};

/// Скользящее окно статистики для одной ссылки.
#[derive(Debug, Clone)]
pub struct SlidingWindowStats {
    /// Кольцевой буфер для 60 секунд.
    /// Индекс = timestamp_sec % 60
    buffer: [u64; 60],
    /// Временная метка последнего обновления (в секундах с UNIX_EPOCH).
    last_update_sec: u64,
    /// Кэшированная сумма за последние 60 секунд.
    /// Обновляется при каждом изменении.
    cached_sum: u64,
    /// Общее количество переходов за всю историю.
    total_hits: u64,
}

impl SlidingWindowStats {
    pub fn new() -> Self {
        Self {
            buffer: [0; 60],
            last_update_sec: 0,
            cached_sum: 0,
            total_hits: 0,
        }
    }

    /// Зарегистрировать хит в текущий момент времени.
    pub fn record_hit(&mut self, now: SystemTime) {
        let now_sec = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Если прошло больше 60 секунд с последнего обновления,
        // нужно очистить буфер
        if now_sec > self.last_update_sec + 60 {
            self.buffer = [0; 60];
            self.cached_sum = 0;
        } else if now_sec > self.last_update_sec {
            // Прошли секунды - обнуляем ячейки
            for sec in (self.last_update_sec + 1)..=now_sec {
                let idx = (sec % 60) as usize;
                self.cached_sum -= self.buffer[idx];
                self.buffer[idx] = 0;
            }
        }

        let idx = (now_sec % 60) as usize;
        self.buffer[idx] += 1;
        self.cached_sum += 1;
        self.total_hits += 1;
        self.last_update_sec = now_sec;
    }

    /// Получить количество переходов за последние 60 секунд.
    pub fn get_last_60s(&self) -> u64 {
        let now_sec = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if now_sec > self.last_update_sec {
            let elapsed = now_sec - self.last_update_sec;
            if elapsed >= 60 {
                return 0;
            }
            // Окно ещё не истекло, возвращаем актуальную сумму
            return self.cached_sum;
        }

        self.cached_sum
    }

    /// Получить общее количество переходов (всего за всю историю).
    pub fn get_total(&self) -> u64 {
        self.total_hits
    }
}

impl Default for SlidingWindowStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Хранилище статистики для всех ссылок.
#[derive(Default)]
pub struct StatsStorage {
    inner: RwLock<HashMap<String, Arc<RwLock<SlidingWindowStats>>>>,
}

impl StatsStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Получить или создать статистику для кода.
    pub fn get_or_create(&self, code: &str) -> Arc<RwLock<SlidingWindowStats>> {
        let mut map = self.inner.write().expect("lock poisoned");
        map.entry(code.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(SlidingWindowStats::new())))
            .clone()
    }

    /// Получить статистику, если существует.
    pub fn get(&self, code: &str) -> Option<Arc<RwLock<SlidingWindowStats>>> {
        let map = self.inner.read().expect("lock poisoned");
        map.get(code).cloned()
    }

    /// Удалить статистику для кода (при удалении ссылки).
    pub fn remove(&self, code: &str) {
        let mut map = self.inner.write().expect("lock poisoned");
        map.remove(code);
    }

    /// Получить топ-N ссылок по общему числу переходов.
    pub fn get_top(&self, limit: usize) -> Vec<(String, u64)> {
        let map = self.inner.read().expect("lock poisoned");
        let mut entries: Vec<_> = map
            .iter()
            .map(|(code, stats)| {
                let total = stats.read().expect("lock poisoned").get_total();
                (code.clone(), total)
            })
            .collect();
        entries.sort_by_key(|(_, total)| Reverse(*total));
        entries.truncate(limit);
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_sliding_window() {
        let mut stats = SlidingWindowStats::new();
        let base = SystemTime::now() - Duration::from_secs(30);

        // Добавляем хиты в последние 30 секунд
        stats.record_hit(base + Duration::from_secs(10));
        stats.record_hit(base + Duration::from_secs(20));
        stats.record_hit(base + Duration::from_secs(30));

        assert_eq!(stats.get_last_60s(), 3);
        assert_eq!(stats.get_total(), 3);

        // Добавляем хит сейчас
        stats.record_hit(SystemTime::now());

        // Должно быть 4 хита
        assert_eq!(stats.get_last_60s(), 4);
        assert_eq!(stats.get_total(), 4);
    }

    #[test]
    fn test_sliding_window_expires() {
        let mut stats = SlidingWindowStats::new();
        let base = SystemTime::now() - Duration::from_secs(120);

        // Добавляем хит 120 секунд назад
        stats.record_hit(base);

        // Окно должно быть пустым
        assert_eq!(stats.get_last_60s(), 0);
        // Но total должен сохраняться
        assert_eq!(stats.get_total(), 1);
    }

    #[test]
    fn test_stats_storage_top() {
        let storage = StatsStorage::new();

        // Создаём статистику для нескольких ссылок
        let stats1 = storage.get_or_create("a");
        stats1.write().unwrap().record_hit(SystemTime::now());
        stats1.write().unwrap().record_hit(SystemTime::now());

        let stats2 = storage.get_or_create("b");
        stats2.write().unwrap().record_hit(SystemTime::now());

        let stats3 = storage.get_or_create("c");
        stats3.write().unwrap().record_hit(SystemTime::now());
        stats3.write().unwrap().record_hit(SystemTime::now());
        stats3.write().unwrap().record_hit(SystemTime::now());

        let top = storage.get_top(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "c");
        assert_eq!(top[0].1, 3);
        assert_eq!(top[1].0, "a");
        assert_eq!(top[1].1, 2);
    }
}
