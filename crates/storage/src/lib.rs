//! In-memory реализации хранилища ссылок.
//!
//! Этот модуль содержит несколько реализаций хранилища для сравнения:
//! - InMemoryRepo - основная реализация с RwLock<HashMap> и AtomicU64
//! - DashMapRepo - альтернативная реализация на шардированной карте
//! - InMemoryRepoV1 - устаревшая версия с write-lock (для бенчмарков)
//! - broken::LostUpdateRepo - демонстрация ошибки check-then-act

use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

use domain::stats::StatsStorage;
use domain::{LinkRepository, LinkStats, RepoError, ShortLink};

pub mod rate_limit_storage;

/// Запись в хранилище: ссылка и атомарный счетчик переходов.
///
/// Используем Arc для разделения между потоками и AtomicU64
/// для атомарного инкремента без блокировок.
pub struct LinkEntry {
    /// Ссылка с метаданными
    link: ShortLink,
    /// Атомарный счетчик переходов
    hits: AtomicU64,
}

impl LinkEntry {
    /// Создает новую запись с нулевым счетчиком.
    fn new(link: ShortLink) -> Arc<Self> {
        Arc::new(Self {
            link,
            hits: AtomicU64::new(0),
        })
    }

    /// Возвращает статистику по ссылке.
    fn stats(&self) -> LinkStats {
        LinkStats {
            link: self.link.clone(),
            // Relaxed порядок достаточен, так как счетчик не синхронизирует
            // другие данные и используется только для статистики
            hits: self.hits.load(Ordering::Relaxed),
        }
    }
}

// ---------------------------------------------------------------------------
// Основная реализация: InMemoryRepo
// ---------------------------------------------------------------------------

mod in_memory {
    use super::*;

    /// Основное хранилище сервиса.
    ///
    /// Структура данных:
    /// - `RwLock<HashMap>` защищает доступ к карте
    /// - `AtomicU64` внутри каждой записи для счетчика переходов
    ///
    /// Преимущества:
    /// - Инкремент счетчика не требует write-lock на всю карту
    /// - Атомарная вставка через entry API (без check-then-act)
    #[derive(Default)]
    pub struct InMemoryRepo {
        /// Защищенная карта ссылок
        inner: RwLock<HashMap<String, Arc<LinkEntry>>>,
        /// Хранилище статистики скользящего окна
        stats: StatsStorage,
    }

    impl InMemoryRepo {
        /// Создает новое пустое хранилище.
        pub fn new() -> Self {
            Self::default()
        }

        /// Синхронная вставка ссылки.
        ///
        /// Использует атомарный entry API для проверки занятости и вставки
        /// в одной операции. Это предотвращает race condition при
        /// параллельных вставках с одинаковым кодом.
        pub fn insert_sync(&self, link: ShortLink) -> Result<(), RepoError> {
            let mut map = self.inner.write().expect("lock poisoned");
            match map.entry(link.code.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    Err(RepoError::CodeTaken(link.code))
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(LinkEntry::new(link));
                    Ok(())
                }
            }
        }

        /// Синхронное получение ссылки по коду.
        pub fn get_sync(&self, code: &str) -> Result<ShortLink, RepoError> {
            let map = self.inner.read().expect("lock poisoned");
            map.get(code)
                .map(|e| e.link.clone())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        /// Синхронное удаление ссылки.
        pub fn remove_sync(&self, code: &str) -> Result<(), RepoError> {
            let mut map = self.inner.write().expect("lock poisoned");
            map.remove(code)
                .map(|_| ())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        /// Синхронная регистрация перехода.
        ///
        /// Важно: инкремент счетчика выполняется атомарно через fetch_add,
        /// что не требует write-lock на всю карту. Это критично для
        /// производительности, так как этот метод вызывается на каждый редирект.
        pub fn record_hit_sync(&self, code: &str) -> Result<u64, RepoError> {
            // read-lock только для поиска записи
            let map = self.inner.read().expect("lock poisoned");
            let entry = map
                .get(code)
                .ok_or_else(|| RepoError::NotFound(code.to_string()))?;

            // Атомарный инкремент без блокировки карты
            let new_hits = entry.hits.fetch_add(1, Ordering::Relaxed) + 1;

            // Обновляем статистику скользящего окна
            let stats = self.stats.get_or_create(code);
            stats
                .write()
                .expect("lock poisoned")
                .record_hit(SystemTime::now());

            Ok(new_hits)
        }

        /// Синхронное получение статистики.
        pub fn stats_sync(&self, code: &str) -> Result<LinkStats, RepoError> {
            let map = self.inner.read().expect("lock poisoned");
            map.get(code)
                .map(|e| e.stats())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        /// Синхронная очистка истекших ссылок.
        ///
        /// Возвращает количество удаленных ссылок.
        /// Этот метод вызывается фоновым уборщиком.
        pub fn purge_expired_sync(&self, now: SystemTime) -> usize {
            let mut map = self.inner.write().expect("lock poisoned");
            let before = map.len();

            // Собираем коды истекших ссылок
            let expired_codes: Vec<String> = map
                .iter()
                .filter(|(_, entry)| entry.link.is_expired(now))
                .map(|(code, _)| code.clone())
                .collect();

            // Удаляем истекшие ссылки и их статистику
            for code in &expired_codes {
                map.remove(code);
                self.stats.remove(code);
            }

            before - map.len()
        }
    }

    /// Асинхронная обертка над синхронными методами.
    #[async_trait::async_trait]
    impl LinkRepository for InMemoryRepo {
        async fn insert(&self, link: ShortLink) -> Result<(), RepoError> {
            self.insert_sync(link)
        }

        async fn get(&self, code: &str) -> Result<ShortLink, RepoError> {
            self.get_sync(code)
        }

        async fn remove(&self, code: &str) -> Result<(), RepoError> {
            self.remove_sync(code)
        }

        async fn record_hit(&self, code: &str) -> Result<u64, RepoError> {
            self.record_hit_sync(code)
        }

        async fn stats(&self, code: &str) -> Result<LinkStats, RepoError> {
            self.stats_sync(code)
        }

        async fn purge_expired(&self, now: SystemTime) -> usize {
            self.purge_expired_sync(now)
        }
    }
}

// ---------------------------------------------------------------------------
// Альтернативная реализация: DashMapRepo
// ---------------------------------------------------------------------------

mod dashmap {
    use super::*;
    use ::dashmap::DashMap;

    /// Хранилище на основе шардированной карты DashMap.
    ///
    /// Преимущества:
    /// - Меньше контента при работе с разными ключами
    /// - Автоматическое шардирование
    ///
    /// Недостатки:
    /// - Нет атомарных операций над несколькими ключами
    /// - Может быть медленнее для одного горячего ключа
    #[derive(Default)]
    pub struct DashMapRepo {
        inner: DashMap<String, Arc<LinkEntry>>,
        stats: StatsStorage,
    }

    impl DashMapRepo {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn insert_sync(&self, link: ShortLink) -> Result<(), RepoError> {
            use ::dashmap::mapref::entry::Entry;

            match self.inner.entry(link.code.clone()) {
                Entry::Occupied(_) => Err(RepoError::CodeTaken(link.code)),
                Entry::Vacant(v) => {
                    v.insert(LinkEntry::new(link));
                    Ok(())
                }
            }
        }

        pub fn get_sync(&self, code: &str) -> Result<ShortLink, RepoError> {
            self.inner
                .get(code)
                .map(|entry| entry.link.clone())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        pub fn remove_sync(&self, code: &str) -> Result<(), RepoError> {
            self.inner
                .remove(code)
                .map(|_| ())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        pub fn record_hit_sync(&self, code: &str) -> Result<u64, RepoError> {
            let entry = self
                .inner
                .get(code)
                .ok_or_else(|| RepoError::NotFound(code.to_string()))?;

            let new_hits = entry.hits.fetch_add(1, Ordering::Relaxed) + 1;

            let stats = self.stats.get_or_create(code);
            stats
                .write()
                .expect("lock poisoned")
                .record_hit(SystemTime::now());

            Ok(new_hits)
        }

        pub fn stats_sync(&self, code: &str) -> Result<LinkStats, RepoError> {
            self.inner
                .get(code)
                .map(|entry: ::dashmap::mapref::one::Ref<'_, String, Arc<LinkEntry>>| entry.stats())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        pub fn purge_expired_sync(&self, now: SystemTime) -> usize {
            let before = self.inner.len();
            let expired_codes: Vec<String> = self
                .inner
                .iter()
                .filter(|entry| entry.link.is_expired(now))
                .map(|entry| entry.key().clone())
                .collect();

            for code in &expired_codes {
                self.inner.remove(code);
                self.stats.remove(code);
            }

            before - self.inner.len()
        }
    }

    #[async_trait::async_trait]
    impl LinkRepository for DashMapRepo {
        async fn insert(&self, link: ShortLink) -> Result<(), RepoError> {
            self.insert_sync(link)
        }

        async fn get(&self, code: &str) -> Result<ShortLink, RepoError> {
            self.get_sync(code)
        }

        async fn remove(&self, code: &str) -> Result<(), RepoError> {
            self.remove_sync(code)
        }

        async fn record_hit(&self, code: &str) -> Result<u64, RepoError> {
            self.record_hit_sync(code)
        }

        async fn stats(&self, code: &str) -> Result<LinkStats, RepoError> {
            self.stats_sync(code)
        }

        async fn purge_expired(&self, now: SystemTime) -> usize {
            self.purge_expired_sync(now)
        }
    }
}

// ---------------------------------------------------------------------------
// V1 реализация (для бенчмарков)
// ---------------------------------------------------------------------------

mod v1 {
    use super::*;

    /// Устаревшая версия хранилища с write-lock на каждый инкремент.
    ///
    /// Используется только для бенчмарков, чтобы показать разницу
    /// в производительности с основной реализацией.
    struct EntryV1 {
        link: ShortLink,
        hits: u64,
    }

    #[derive(Default)]
    pub struct InMemoryRepoV1 {
        inner: RwLock<HashMap<String, EntryV1>>,
    }

    impl InMemoryRepoV1 {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn insert(&self, link: ShortLink) -> Result<(), RepoError> {
            let mut map = self.inner.write().expect("lock poisoned");
            match map.entry(link.code.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    Err(RepoError::CodeTaken(link.code))
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(EntryV1 { link, hits: 0 });
                    Ok(())
                }
            }
        }

        pub fn get(&self, code: &str) -> Result<ShortLink, RepoError> {
            let map = self.inner.read().expect("lock poisoned");
            map.get(code)
                .map(|e| e.link.clone())
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }

        /// В этой версии инкремент требует write-lock на всю карту.
        /// Это медленнее, чем атомарный инкремент в основной реализации.
        pub fn record_hit(&self, code: &str) -> Result<u64, RepoError> {
            let mut map = self.inner.write().expect("lock poisoned");
            let entry = map
                .get_mut(code)
                .ok_or_else(|| RepoError::NotFound(code.to_string()))?;
            entry.hits += 1;
            Ok(entry.hits)
        }

        pub fn stats(&self, code: &str) -> Result<LinkStats, RepoError> {
            let map = self.inner.read().expect("lock poisoned");
            map.get(code)
                .map(|e| LinkStats {
                    link: e.link.clone(),
                    hits: e.hits,
                })
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// Намеренно сломанная версия (демонстрация)
// ---------------------------------------------------------------------------

pub mod broken {
    //! Демонстрация ошибок конкурентности.
    //!
    //! Эта реализация содержит известную ошибку check-then-act
    //! в методе record_hit. Используется для демонстрации важности
    //! атомарных операций.
    use domain::{LinkStats, RepoError, ShortLink};
    use std::{collections::HashMap, sync::RwLock};

    struct Entry {
        link: ShortLink,
        hits: u64,
    }

    #[derive(Default)]
    pub struct LostUpdateRepo {
        inner: RwLock<HashMap<String, Entry>>,
    }

    impl LostUpdateRepo {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn insert(&self, link: ShortLink) -> Result<(), RepoError> {
            let mut map = self.inner.write().expect("lock poisoned");
            match map.entry(link.code.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    Err(RepoError::CodeTaken(link.code))
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(Entry { link, hits: 0 });
                    Ok(())
                }
            }
        }

        /// ОШИБКА: read-modify-write без атомарности.
        ///
        /// Между чтением значения и его записью другой поток может
        /// изменить счетчик, что приведет к потере обновлений.
        pub fn record_hit(&self, code: &str) -> Result<u64, RepoError> {
            // Шаг 1: читаем значение под read-lock
            let current = {
                let map = self.inner.read().expect("lock poisoned");
                map.get(code)
                    .map(|e| e.hits)
                    .ok_or_else(|| RepoError::NotFound(code.to_string()))?
            };
            // lock отпущен - здесь другой поток может изменить значение
            std::hint::spin_loop();
            // Шаг 2: записываем current + 1 под write-lock
            // Но если другой поток уже изменил значение, мы его потеряем
            let mut map = self.inner.write().expect("lock poisoned");
            let entry = map
                .get_mut(code)
                .ok_or_else(|| RepoError::NotFound(code.to_string()))?;
            entry.hits = current + 1;
            Ok(entry.hits)
        }

        pub fn stats(&self, code: &str) -> Result<LinkStats, RepoError> {
            let map = self.inner.read().expect("lock poisoned");
            map.get(code)
                .map(|e| LinkStats {
                    link: e.link.clone(),
                    hits: e.hits,
                })
                .ok_or_else(|| RepoError::NotFound(code.to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// Экспорты
// ---------------------------------------------------------------------------

pub use dashmap::DashMapRepo;
pub use in_memory::InMemoryRepo;
pub use v1::InMemoryRepoV1;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[tokio::test]
    async fn test_in_memory_repo() {
        let repo = InMemoryRepo::new();
        let link = ShortLink::new("test", "https://example.com");

        repo.insert(link.clone()).await.unwrap();

        let dup = repo
            .insert(ShortLink::new("test", "https://other.com"))
            .await;
        assert_eq!(dup, Err(RepoError::CodeTaken("test".to_string())));

        let retrieved = repo.get("test").await.unwrap();
        assert_eq!(retrieved.target_url, "https://example.com");

        let hits = repo.record_hit("test").await.unwrap();
        assert_eq!(hits, 1);

        let stats = repo.stats("test").await.unwrap();
        assert_eq!(stats.hits, 1);

        repo.remove("test").await.unwrap();
        let err = repo.get("test").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_purge_expired() {
        let repo = InMemoryRepo::new();
        let now = SystemTime::now();

        repo.insert(
            ShortLink::new("old", "https://example.com/old")
                .with_expires_at(now - Duration::from_secs(60)),
        )
        .await
        .unwrap();

        repo.insert(
            ShortLink::new("fresh", "https://example.com/fresh")
                .with_expires_at(now + Duration::from_secs(60)),
        )
        .await
        .unwrap();

        let removed = repo.purge_expired(now).await;
        assert_eq!(removed, 1);

        assert!(repo.get("old").await.is_err());
        assert!(repo.get("fresh").await.is_ok());
    }
}
