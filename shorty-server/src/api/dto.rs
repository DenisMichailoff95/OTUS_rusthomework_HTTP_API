//! DTO — wire-формат API.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use domain::LinkStats;
use domain::ShortLink;
use serde::{Deserialize, Serialize};
use url::Url;
use utoipa::ToSchema;

use super::error::AppError;

/// Тело `POST /api/v1/links`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CreateLinkRequest {
    /// Целевой URL
    #[schema(example = "https://example.com")]
    pub target_url: String,
    /// Кастомный код (опционально, 4-32 символа, только [a-zA-Z0-9_-])
    #[schema(example = "promo2026")]
    pub custom_code: Option<String>,
    /// TTL в секундах (опционально, минимум 60)
    #[schema(example = 3600)]
    pub ttl_seconds: Option<u64>,
}

/// Тело `PUT /api/v1/links/{code}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UpdateLinkRequest {
    /// Новый целевой URL
    #[schema(example = "https://example.com/updated")]
    pub target_url: String,
    /// Версия для optimistic locking
    #[schema(example = 1)]
    pub version: i64,
}

/// Представление ссылки в ответах API.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
#[serde(rename_all = "snake_case")]
pub struct LinkResponse {
    /// Короткий код
    #[schema(example = "abc123")]
    pub code: String,
    /// Целевой URL
    #[schema(example = "https://example.com")]
    pub target_url: String,
    /// Время создания (Unix timestamp)
    #[schema(example = 1700000000)]
    pub created_at_unix: u64,
    /// Время истечения (Unix timestamp, опционально)
    #[schema(example = 1700003600)]
    pub expires_at_unix: Option<u64>,
    /// Количество переходов
    #[schema(example = 42)]
    pub hits: u64,
    /// Версия для optimistic locking
    #[schema(example = 1)]
    pub version: i64,
}

/// Ответ со списком ссылок.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ListLinksResponse {
    /// Список ссылок
    pub links: Vec<LinkResponse>,
    /// Курсор для следующей страницы
    pub next_cursor: Option<String>,
}

impl From<LinkStats> for LinkResponse {
    fn from(stats: LinkStats) -> Self {
        Self {
            code: stats.link.code,
            target_url: stats.link.target_url,
            created_at_unix: unix_secs(stats.link.created_at),
            expires_at_unix: stats.link.expires_at.map(unix_secs),
            hits: stats.hits,
            version: stats.link.version,
        }
    }
}

impl From<ShortLink> for LinkResponse {
    fn from(link: ShortLink) -> Self {
        Self {
            code: link.code,
            target_url: link.target_url,
            created_at_unix: unix_secs(link.created_at),
            expires_at_unix: link.expires_at.map(unix_secs),
            hits: 0,
            version: link.version,
        }
    }
}

pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// «Parse, don't validate»: не проверяем строку, а превращаем её в `Url`.
pub fn parse_target_url(raw: &str) -> Result<Url, AppError> {
    let url =
        Url::parse(raw).map_err(|e| AppError::Validation(format!("invalid target_url: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Validation(format!(
            "target_url must use http or https, got '{}'",
            url.scheme()
        )));
    }
    Ok(url)
}

/// Правила `custom_code`: 4..=32 символа из `[a-zA-Z0-9_-]`.
pub fn validate_custom_code(code: &str) -> Result<(), AppError> {
    if !(4..=32).contains(&code.len()) {
        return Err(AppError::Validation(
            "custom_code must be 4..=32 characters long".to_string(),
        ));
    }
    if !code
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AppError::Validation(
            "custom_code may contain only [a-zA-Z0-9_-]".to_string(),
        ));
    }
    Ok(())
}

/// TTL → абсолютный момент истечения.
/// TTL должен быть >= 60 секунд (минимальное значение).
pub fn expires_at_from_ttl(ttl_seconds: Option<u64>) -> Result<Option<SystemTime>, AppError> {
    match ttl_seconds {
        None => Ok(None),
        Some(0) => Err(AppError::Validation(
            "ttl_seconds must be greater than zero".to_string(),
        )),
        Some(secs) if secs < 60 => Err(AppError::Validation(
            "ttl_seconds must be at least 60 seconds".to_string(),
        )),
        Some(secs) => Ok(Some(SystemTime::now() + Duration::from_secs(secs))),
    }
}
