//! Обработчики HTTP запросов.

use std::time::SystemTime;

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use domain::{RepoError, ShortLink};
use serde::Serialize;
use storage::link_key;
use utoipa::ToSchema;

use super::{
    dto::{
        CreateLinkRequest, LinkResponse, ListLinksResponse, UpdateLinkRequest, expires_at_from_ttl,
        parse_target_url, unix_secs, validate_custom_code,
    },
    error::{AppError, AppJson, ErrorBody},
};
use crate::AppState;

use base64::Engine;

// ---------------------------------------------------------------------------
// CRUD операции со ссылками
// ---------------------------------------------------------------------------

/// Создать новую короткую ссылку.
#[utoipa::path(
    post,
    path = "/api/v1/links",
    tags = ["shorty"],
    request_body = CreateLinkRequest,
    responses(
        (status = 201, description = "Ссылка создана", body = LinkResponse),
        (status = 400, description = "Некорректный запрос", body = ErrorBody),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 409, description = "Код уже занят", body = ErrorBody),
        (status = 503, description = "БД недоступна", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
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

/// Обновить ссылку с optimistic locking.
#[utoipa::path(
    put,
    path = "/api/v1/links/{code}",
    tags = ["shorty"],
    params(
        ("code" = String, Path, description = "Код ссылки")
    ),
    request_body = UpdateLinkRequest,
    responses(
        (status = 200, description = "Ссылка обновлена", body = LinkResponse),
        (status = 400, description = "Некорректный запрос", body = ErrorBody),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 403, description = "Доступ запрещён", body = ErrorBody),
        (status = 404, description = "Ссылка не найдена", body = ErrorBody),
        (status = 409, description = "Конфликт версий", body = ErrorBody),
        (status = 503, description = "БД недоступна", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
pub async fn update_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
    AppJson(req): AppJson<UpdateLinkRequest>,
) -> Result<Json<LinkResponse>, AppError> {
    let target_url = parse_target_url(&req.target_url)?;
    let link = state
        .repo
        .update(&code, target_url.as_str(), req.version)
        .await?;

    state.cache.invalidate(&link_key(&code)).await;

    Ok(Json(LinkResponse {
        code: link.code,
        target_url: link.target_url,
        created_at_unix: unix_secs(link.created_at),
        expires_at_unix: link.expires_at.map(unix_secs),
        hits: 0,
        version: link.version,
    }))
}

/// Получить ссылку по коду (с кешированием).
#[utoipa::path(
    get,
    path = "/api/v1/links/{code}",
    tags = ["shorty"],
    params(
        ("code" = String, Path, description = "Код ссылки")
    ),
    responses(
        (status = 200, description = "Ссылка найдена", body = LinkResponse),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 403, description = "Доступ запрещён", body = ErrorBody),
        (status = 404, description = "Ссылка не найдена", body = ErrorBody),
        (status = 503, description = "БД недоступна", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
pub async fn get_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<LinkResponse>, AppError> {
    let key = link_key(&code);

    if let Some(cached) = state.cache.get::<LinkResponse>(&key).await {
        tracing::debug!(code = %code, "cache hit");
        return Ok(Json(cached));
    }

    tracing::debug!(code = %code, "cache miss");

    let stats = state.repo.stats(&code).await?;
    let response: LinkResponse = stats.into();

    state.cache.set(&key, &response).await;

    Ok(Json(response))
}

/// Получить список ссылок с keyset-пагинацией.
#[utoipa::path(
    get,
    path = "/api/v1/links",
    tags = ["shorty"],
    params(
        ("limit" = Option<u64>, Query, description = "Лимит страницы (макс 100)"),
        ("cursor" = Option<String>, Query, description = "Курсор для пагинации")
    ),
    responses(
        (status = 200, description = "Список ссылок", body = ListLinksResponse),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 403, description = "Доступ запрещён", body = ErrorBody),
        (status = 503, description = "БД недоступна", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
pub async fn list_links(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ListLinksResponse>, AppError> {
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u64>().ok())
        .unwrap_or(20)
        .min(100);

    let cursor = params.get("cursor").and_then(|c| {
        let decoded = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(c)
            .ok()?;
        let decoded_str = String::from_utf8(decoded).ok()?;
        serde_json::from_str::<(String, String)>(&decoded_str).ok()
    });

    let (links, next_cursor) = state
        .repo
        .list(
            limit,
            cursor.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
        )
        .await?;

    let response = ListLinksResponse {
        links: links.into_iter().map(LinkResponse::from).collect(),
        next_cursor: next_cursor.map(|(ts, code)| {
            let json = serde_json::to_string(&(ts, code)).unwrap();
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(json)
        }),
    };

    Ok(Json(response))
}

/// Редирект по короткому коду (публичный).
#[utoipa::path(
    get,
    path = "/{code}",
    tags = ["shorty"],
    params(
        ("code" = String, Path, description = "Код ссылки")
    ),
    responses(
        (status = 302, description = "Редирект на целевой URL"),
        (status = 404, description = "Ссылка не найдена", body = ErrorBody),
        (status = 503, description = "БД недоступна", body = ErrorBody),
    )
)]
pub async fn redirect(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let link = state.repo.get(&code).await?;
    if link.is_expired(SystemTime::now()) {
        return Err(AppError::NotFound);
    }
    state.repo.record_hit(&code).await?;

    // Инвалидируем кеш после обновления счетчика
    state.cache.invalidate(&link_key(&code)).await;

    Ok((
        StatusCode::TEMPORARY_REDIRECT,
        [(header::LOCATION, link.target_url)],
    ))
}

/// Удалить ссылку.
#[utoipa::path(
    delete,
    path = "/api/v1/links/{code}",
    tags = ["shorty"],
    params(
        ("code" = String, Path, description = "Код ссылки")
    ),
    responses(
        (status = 204, description = "Ссылка удалена"),
        (status = 401, description = "Требуется аутентификация", body = ErrorBody),
        (status = 403, description = "Доступ запрещён", body = ErrorBody),
        (status = 404, description = "Ссылка не найдена", body = ErrorBody),
        (status = 503, description = "БД недоступна", body = ErrorBody),
    ),
    security(("bearerAuth" = []))
)]
pub async fn delete_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<StatusCode, AppError> {
    state.repo.remove(&code).await?;

    // Инвалидируем кеш после удаления
    state.cache.invalidate(&link_key(&code)).await;

    Ok(StatusCode::NO_CONTENT)
}

/// Обработчик для неизвестных путей.
pub async fn fallback_404() -> AppError {
    AppError::NotFound
}

// ---------------------------------------------------------------------------
// Вспомогательные функции
// ---------------------------------------------------------------------------

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

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub status: &'static str,
}

#[derive(Serialize, ToSchema)]
pub struct Version {
    pub version: &'static str,
}

/// Health check эндпоинт (публичный).
#[utoipa::path(
    get,
    path = "/healthz",
    tags = ["health"],
    responses(
        (status = 200, description = "Сервис жив", body = Health)
    )
)]
pub async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Версия сервиса (публичный).
#[utoipa::path(
    get,
    path = "/version",
    tags = ["health"],
    responses(
        (status = 200, description = "Версия сервиса", body = Version)
    )
)]
pub async fn version() -> Json<Version> {
    Json(Version {
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub async fn slow() -> &'static str {
    tokio::task::spawn_blocking(|| {
        std::thread::sleep(std::time::Duration::from_secs(2));
    })
    .await
    .expect("blocking task panicked");
    "done: spawn_blocking kept workers free\n"
}

pub async fn slow_blocking() -> &'static str {
    std::thread::sleep(std::time::Duration::from_secs(2));
    "done: but a worker thread was blocked for 2s!\n"
}
