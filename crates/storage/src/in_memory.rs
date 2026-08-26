//! In-memory реализация репозитория ссылок.

use std::{
    collections::HashMap,
    sync::{
        RwLock,
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
    fn new(link: ShortLink) -> Self {
        Self {
            link,
            hits: AtomicU64::new(0),
        }
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
    inner: RwLock<HashMap<String, LinkEntry>>,
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

    pub fn update_sync(&self, code: &str, target_url: &str) -> Result<ShortLink, RepoError> {
        let mut map = self.inner.write().expect("lock poisoned");
        let entry = map
            .get_mut(code)
            .ok_or_else(|| RepoError::NotFound(code.to_string()))?;
        entry.link.target_url = target_url.to_string();
        entry.link.updated_at = SystemTime::now();
        entry.link.version += 1;
        Ok(entry.link.clone())
    }

    pub fn list_sync(
        &self,
        limit: usize,
        cursor: Option<(&str, &str)>,
    ) -> (Vec<ShortLink>, Option<(String, String)>) {
        use std::time::UNIX_EPOCH;
        let mut entries: Vec<_> = self
            .inner
            .read()
            .expect("lock poisoned")
            .values()
            .map(|e| e.link.clone())
            .collect();

        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.code.cmp(&a.code)));

        let start = match cursor {
            Some((c_ts, c_code)) => entries
                .iter()
                .position(|link| {
                    link.created_at
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string()
                        == c_ts
                        && link.code == c_code
                })
                .map(|i| i + 1)
                .unwrap_or(0),
            None => 0,
        };

        let end = (start + limit).min(entries.len());
        let page = entries[start..end].to_vec();

        let next_cursor = page.last().map(|link| {
            let ts = link
                .created_at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string();
            (ts, link.code.clone())
        });

        (page, next_cursor)
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

    async fn update(
        &self,
        code: &str,
        target_url: &str,
        _version: i64,
    ) -> Result<ShortLink, RepoError> {
        self.update_sync(code, target_url)
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

    async fn list(
        &self,
        limit: u64,
        cursor: Option<(&str, &str)>,
    ) -> Result<(Vec<ShortLink>, Option<(String, String)>), RepoError> {
        Ok(self.list_sync(limit as usize, cursor))
    }

    async fn purge_expired(&self, now: SystemTime) -> usize {
        self.purge_expired_sync(now)
    }
}
