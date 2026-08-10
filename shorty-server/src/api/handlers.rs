//! Обработчики HTTP запросов.
//!
//! Каждый handler отвечает за конкретный endpoint и
//! делегирует логику в доменный слой через хранилище.

use std::time::{Duration, SystemTime};

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use domain::{RepoError, ShortLink};
use serde::Serialize;

use super::{
    dto::{
        CreateLinkRequest, LinkResponse, expires_at_from_ttl, parse_target_url,
        validate_custom_code,
    },
    error::{AppError, AppJson},
};
use crate::AppState;

// ---------------------------------------------------------------------------
// CRUD операции со ссылками
// ---------------------------------------------------------------------------

/// Создание новой короткой ссылки.
///
/// Если `custom_code` не указан, генерируется автоматически.
/// При указании `ttl_seconds` ссылка будет автоматически удалена
/// фоновым процессом после истечения срока.
///
/// # Ошибки
/// - 409 - код уже занят
/// - 422 - невалидный URL или код
/// - 429 - превышен rate limit
pub async fn create_link(
    State(state): State<AppState>,
    AppJson(req): AppJson<CreateLinkRequest>,
) -> Result<impl IntoResponse, AppError> {
    let target_url = parse_target_url(&req.target_url)?;
    let expires_at = expires_at_from_ttl(req.ttl_seconds)?;

    let code = match req.custom_code {
        Some(code) => {
            validate_custom_code(&code)?;
            insert_link(&state, &code, target_url.as_str(), expires_at).await?;
            code
        }
        None => generate_code(&state, target_url.as_str(), expires_at).await?,
    };

    let stats = state.repo.stats(&code).await?;
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, format!("/api/v1/links/{code}"))],
        Json(LinkResponse::from(stats)),
    ))
}

/// Получение метаданных ссылки.
///
/// Возвращает информацию о ссылке включая количество переходов.
pub async fn get_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<LinkResponse>, AppError> {
    let stats = state.repo.stats(&code).await?;
    Ok(Json(stats.into()))
}

/// Редирект по короткому коду.
///
/// Автоматически инкрементирует счетчик переходов.
/// Возвращает 307 Temporary Redirect для сохранения метода запроса.
pub async fn redirect(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let link = state.repo.get(&code).await?;
    if link.is_expired(SystemTime::now()) {
        return Err(AppError::NotFound);
    }
    state.repo.record_hit(&code).await?;
    Ok((
        StatusCode::TEMPORARY_REDIRECT,
        [(header::LOCATION, link.target_url)],
    ))
}

/// Удаление ссылки.
///
/// # Контракт
/// - 204 - успешное удаление
/// - 404 - ссылка не найдена
pub async fn delete_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<StatusCode, AppError> {
    state.repo.remove(&code).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Обработчик для неизвестных путей.
/// Возвращает 404 в едином формате ошибок.
pub async fn fallback_404() -> AppError {
    AppError::NotFound
}

// ---------------------------------------------------------------------------
// Вспомогательные функции
// ---------------------------------------------------------------------------

/// Генерация уникального кода с повторными попытками.
///
/// Использует атомарную операцию вставки для проверки занятости.
async fn generate_code(
    state: &AppState,
    target_url: &str,
    expires_at: Option<SystemTime>,
) -> Result<String, AppError> {
    for _ in 0..state.config.max_generate_attempts {
        let code = nanoid::nanoid!(state.config.code_length);
        match try_insert_link(state, &code, target_url, expires_at).await {
            Ok(()) => return Ok(code),
            Err(RepoError::CodeTaken(_)) => continue,
            Err(other) => return Err(other.into()),
        }
    }
    Err(AppError::Internal(anyhow::anyhow!(
        "failed to generate a unique code in {} attempts",
        state.config.max_generate_attempts
    )))
}

/// Атомарная вставка ссылки с проверкой занятости кода.
async fn insert_link(
    state: &AppState,
    code: &str,
    target_url: &str,
    expires_at: Option<SystemTime>,
) -> Result<(), AppError> {
    try_insert_link(state, code, target_url, expires_at)
        .await
        .map_err(Into::into)
}

/// Внутренняя функция атомарной вставки.
async fn try_insert_link(
    state: &AppState,
    code: &str,
    target_url: &str,
    expires_at: Option<SystemTime>,
) -> Result<(), RepoError> {
    let mut link = ShortLink::new(code, target_url);
    if let Some(at) = expires_at {
        link = link.with_expires_at(at);
    }
    state.repo.insert(link).await
}

// ---------------------------------------------------------------------------
// Технические эндпоинты
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Health {
    status: &'static str,
}

#[derive(Serialize)]
pub struct Version {
    version: &'static str,
}

/// Проверка работоспособности сервиса.
pub async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Версия сервиса из Cargo.toml.
pub async fn version() -> Json<Version> {
    Json(Version {
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Демонстрация корректной обработки блокирующих операций.
pub async fn slow() -> &'static str {
    tokio::task::spawn_blocking(|| {
        std::thread::sleep(Duration::from_secs(2));
    })
    .await
    .expect("blocking task panicked");
    "done: spawn_blocking kept workers free\n"
}

/// Демонстрация НЕПРАВИЛЬНОЙ обработки блокирующих операций.
/// Этот метод блокирует поток выполнения и не должен использоваться в продакшене.
pub async fn slow_blocking() -> &'static str {
    std::thread::sleep(Duration::from_secs(2));
    "done: but a worker thread was blocked for 2s!\n"
}
