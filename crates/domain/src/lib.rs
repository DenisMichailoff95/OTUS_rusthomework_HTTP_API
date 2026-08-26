//! Доменный слой сервиса сокращения ссылок.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use uuid::Uuid;

pub mod rate_limit;
pub mod stats;

/// Короткая ссылка - основная сущность сервиса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortLink {
    /// Уникальный идентификатор в БД
    pub id: Option<Uuid>,
    /// Уникальный короткий код для редиректа
    pub code: String,
    /// Целевой URL для перенаправления
    pub target_url: String,
    /// Время создания ссылки
    pub created_at: SystemTime,
    /// Время обновления
    pub updated_at: SystemTime,
    /// Опциональное время истечения срока действия
    pub expires_at: Option<SystemTime>,
    /// Версия для optimistic locking
    pub version: i64,
}

/// Статистика переходов по ссылке
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStats {
    /// Сама ссылка
    pub link: ShortLink,
    /// Общее количество переходов
    pub hits: u64,
}

/// Ошибки, возникающие при работе с хранилищем
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RepoError {
    /// Попытка использовать уже занятый код
    #[error("code '{0}' is already taken")]
    CodeTaken(String),
    /// Ссылка с указанным кодом не найдена
    #[error("link '{0}' not found")]
    NotFound(String),
    /// Внутренняя ошибка
    #[error("internal error")]
    Internal(#[from] anyhow::Error), // <-- теперь anyhow доступен
}

/// Контракт хранилища ссылок.
#[async_trait::async_trait]
pub trait LinkRepository: Send + Sync {
    async fn insert(&self, link: ShortLink) -> Result<(), RepoError>;
    async fn get(&self, code: &str) -> Result<ShortLink, RepoError>;
    async fn remove(&self, code: &str) -> Result<(), RepoError>;
    async fn record_hit(&self, code: &str) -> Result<u64, RepoError>;
    async fn stats(&self, code: &str) -> Result<LinkStats, RepoError>;
    async fn purge_expired(&self, now: SystemTime) -> usize;
}

impl ShortLink {
    pub fn new(code: impl Into<String>, target_url: impl Into<String>) -> Self {
        let now = SystemTime::now();
        Self {
            id: None,
            code: code.into(),
            target_url: target_url.into(),
            created_at: now,
            updated_at: now,
            expires_at: None,
            version: 1,
        }
    }

    pub fn with_expires_at(mut self, expires_at: SystemTime) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.expires_at.is_some_and(|exp| exp <= now)
    }
}
