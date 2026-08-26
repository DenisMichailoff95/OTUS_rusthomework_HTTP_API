//! Единый слой ошибок API.
//!
//! Все ошибки в сервисе преобразуются в этот тип и возвращаются
//! в едином JSON формате. Это обеспечивает консистентность API
//! и упрощает обработку ошибок на клиенте.

use axum::{
    Json,
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::request_id::current_request_id;

/// Ошибка уровня приложения.
///
/// Каждый вариант ошибки маппится на HTTP статус и JSON-тело.
/// Внутренние детали ошибок (например, паники) никогда не утекают
/// наружу в ответе, только в логи.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("resource not found")]
    NotFound,

    #[error("code is already taken")]
    CodeTaken,

    #[error("version conflict")]
    VersionConflict,

    #[error("storage unavailable")]
    Unavailable,

    #[error("{0}")]
    Validation(String),

    /// Отказ extractor'а (битый JSON, лишние поля) → статус из rejection.
    #[error("{message}")]
    InvalidBody { status: StatusCode, message: String },

    /// Превышен rate limit → 429 с заголовком Retry-After.
    #[error("rate limit exceeded, retry after {retry_after} seconds")]
    RateLimitExceeded { retry_after: u64 },

    /// Внутренняя ошибка → 500.
    /// Детали уходят только в лог, клиенту возвращается общее сообщение.
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

/// Стандартное тело ошибки для всех ответов.
///
/// Все ошибки возвращаются в этом формате, что делает API предсказуемым
/// и упрощает обработку ошибок на клиенте.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Машиночитаемый код ошибки (например, "not_found", "rate_limit_exceeded")
    pub code: String,
    /// Человекочитаемое сообщение об ошибке
    pub message: String,
    /// Идентификатор запроса для трассировки в логах
    pub request_id: Option<String>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Получаем request_id из task-local хранилища
        // Это позволяет нам добавлять id в тело ошибки без явной передачи
        // через все handlers
        let request_id = current_request_id();

        let (status, code) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AppError::CodeTaken => (StatusCode::CONFLICT, "code_taken"),
            AppError::VersionConflict => (StatusCode::CONFLICT, "version_conflict"),
            AppError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            AppError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_error"),
            AppError::InvalidBody { status, .. } => (
                *status,
                match *status {
                    StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
                    StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
                    StatusCode::UNPROCESSABLE_ENTITY => "validation_error",
                    _ => "bad_request",
                },
            ),
            // Для rate limit добавляем заголовок Retry-After
            AppError::RateLimitExceeded { retry_after } => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(axum::http::header::RETRY_AFTER, retry_after.to_string())],
                    Json(ErrorBody {
                        code: "rate_limit_exceeded".to_string(),
                        message: format!("rate limit exceeded, retry after {retry_after} seconds"),
                        request_id,
                    }),
                )
                    .into_response();
            }
            // Внутренние ошибки логируем с полным стеком, но клиенту
            // отдаем только общее сообщение
            AppError::Internal(err) => {
                tracing::error!(error = ?err, request_id = ?request_id, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };

        let body = ErrorBody {
            code: code.to_string(),
            message: self.to_string(),
            request_id,
        };
        (status, Json(body)).into_response()
    }
}

/// Преобразование ошибок репозитория в ошибки API.
impl From<domain::RepoError> for AppError {
    fn from(err: domain::RepoError) -> Self {
        match err {
            domain::RepoError::NotFound(_) => AppError::NotFound,
            domain::RepoError::CodeTaken(_) => AppError::CodeTaken,
            domain::RepoError::VersionConflict => AppError::VersionConflict,
            domain::RepoError::Unavailable => AppError::Unavailable,
            domain::RepoError::Internal(_) => AppError::Internal(anyhow::anyhow!("internal error")),
        }
    }
}

/// Обёртка для JSON с кастомной ошибкой.
///
/// Позволяет преобразовывать ошибки десериализации axum в наш формат.
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(AppJson(value)),
            Err(rejection) => Err(AppError::InvalidBody {
                status: rejection.status(),
                message: rejection.body_text(),
            }),
        }
    }
}
