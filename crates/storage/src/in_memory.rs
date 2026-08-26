//! In-memory реализация репозитория ссылок.

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

/// Запись в хранилище: ссылка и атомарный счетчик переходов.
pub struct LinkEntry {
    pub link: ShortLink,
    pub hits: AtomicU64,
}

impl LinkEntry {
    fn new(link: ShortLink) -> Arc<Self> {
        Arc::new(Self {
            link,
            hits: AtomicU64::new(0),
        })
    }

    fn stats(&self) -> LinkStats {
        LinkStats {
            link: self.link.clone(),
            hits: self.hits.load(Ordering::Relaxed),
        }
    }
}

/// Основное хранилище сервиса.
#[derive(Default)]
pub struct InMemoryRepo {
    inner: RwLock<HashMap<String, Arc<LinkEntry>>>,
    stats: StatsStorage,
}

impl InMemoryRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_sync(&self, link: ShortLink) -> Result<(), RepoError> {
        let mut map = self.inner.write().expect("lock poisoned");
        match map.entry(link.code.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => Err(RepoError::CodeTaken(link.code)),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(LinkEntry::new(link));
                Ok(())
            }
        }
    }

    pub fn get_sync(&self, code: &str) -> Result<ShortLink, RepoError> {
        let map = self.inner.read().expect("lock poisoned");
        map.get(code)
            .map(|e| e.link.clone())
            .ok_or_else(|| RepoError::NotFound(code.to_string()))
    }

    pub fn remove_sync(&self, code: &str) -> Result<(), RepoError> {
        let mut map = self.inner.write().expect("lock poisoned");
        map.remove(code)
            .map(|_| ())
            .ok_or_else(|| RepoError::NotFound(code.to_string()))
    }

    pub fn record_hit_sync(&self, code: &str) -> Result<u64, RepoError> {
        let map = self.inner.read().expect("lock poisoned");
        let entry = map
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
        let map = self.inner.read().expect("lock poisoned");
        map.get(code)
            .map(|e| e.stats())
            .ok_or_else(|| RepoError::NotFound(code.to_string()))
    }

    pub fn purge_expired_sync(&self, now: SystemTime) -> usize {
        let mut map = self.inner.write().expect("lock poisoned");
        let before = map.len();

        let expired_codes: Vec<String> = map
            .iter()
            .filter(|(_, entry)| entry.link.is_expired(now))
            .map(|(code, _)| code.clone())
            .collect();

        for code in &expired_codes {
            map.remove(code);
            self.stats.remove(code);
        }

        before - map.len()
    }
}

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
