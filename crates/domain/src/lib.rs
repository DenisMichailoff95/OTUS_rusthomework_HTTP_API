//! Доменный слой сервиса сокращения ссылок.
//!
//! Этот модуль содержит основные бизнес-сущности и контракты,
//! которые не зависят от внешних фреймворков и библиотек.

use std::time::SystemTime;

pub mod rate_limit;
pub mod stats;

/// Короткая ссылка - основная сущность сервиса.
///
/// Содержит информацию о коротком коде, целевом URL,
/// времени создания и опциональном сроке жизни.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortLink {
    /// Уникальный короткий код для редиректа
    pub code: String,
    /// Целевой URL для перенаправления
    pub target_url: String,
    /// Время создания ссылки (Unix timestamp)
    pub created_at: SystemTime,
    /// Опциональное время истечения срока действия
    pub expires_at: Option<SystemTime>,
}

/// Статистика переходов по ссылке
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// Контракт хранилища ссылок.
///
/// Определяет набор операций для работы с ссылками,
/// включая создание, получение, удаление и подсчет переходов.
#[async_trait::async_trait]
pub trait LinkRepository: Send + Sync {
    /// Сохранить новую ссылку.
    ///
    /// # Ошибки
    /// - `CodeTaken` - если код уже занят другой ссылкой
    async fn insert(&self, link: ShortLink) -> Result<(), RepoError>;

    /// Получить ссылку по коду.
    ///
    /// # Ошибки
    /// - `NotFound` - если ссылка не найдена
    async fn get(&self, code: &str) -> Result<ShortLink, RepoError>;

    /// Удалить ссылку по коду.
    ///
    /// # Ошибки
    /// - `NotFound` - если ссылка не найдена
    async fn remove(&self, code: &str) -> Result<(), RepoError>;

    /// Зарегистрировать переход по ссылке.
    ///
    /// # Возвращает
    /// Новое значение счетчика переходов
    ///
    /// # Ошибки
    /// - `NotFound` - если ссылка не найдена
    async fn record_hit(&self, code: &str) -> Result<u64, RepoError>;

    /// Получить ссылку вместе со статистикой переходов.
    ///
    /// # Ошибки
    /// - `NotFound` - если ссылка не найдена
    async fn stats(&self, code: &str) -> Result<LinkStats, RepoError>;

    /// Удалить все ссылки с истекшим сроком действия.
    ///
    /// # Возвращает
    /// Количество удаленных ссылок
    async fn purge_expired(&self, now: SystemTime) -> usize;
}

impl ShortLink {
    /// Создает новую ссылку с текущим временем создания.
    pub fn new(code: impl Into<String>, target_url: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            target_url: target_url.into(),
            created_at: SystemTime::now(),
            expires_at: None,
        }
    }

    /// Устанавливает срок жизни ссылки.
    #[must_use]
    pub fn with_expires_at(mut self, expires_at: SystemTime) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Проверяет, истек ли срок действия ссылки.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.expires_at.is_some_and(|exp| exp <= now)
    }
}
