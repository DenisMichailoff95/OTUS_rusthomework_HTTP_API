//! Единый слой ошибок API.

use axum::{
    Json,
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::request_id::current_request_id;

/// Стандартное тело ошибки для всех ответов.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// Машиночитаемый код ошибки
    #[schema(example = "not_found")]
    pub code: String,
    /// Человекочитаемое сообщение об ошибке
    #[schema(example = "resource not found")]
    pub message: String,
    /// Идентификатор запроса для трассировки в логах
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    pub request_id: Option<String>,
    /// Детали валидации (опционально)
    #[schema(example = json!([{"field": "target_url", "issue": "required"}]))]
    pub details: Option<Vec<FieldError>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FieldError {
    /// Имя проблемного поля
    #[schema(example = "target_url")]
    pub field: String,
    /// Описание проблемы
    #[schema(example = "required")]
    pub issue: String,
}

/// Ошибка уровня приложения.
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

    #[error("{message}")]
    Validation {
        message: String,
        details: Vec<FieldError>,
    },

    #[error("{message}")]
    InvalidBody { status: StatusCode, message: String },

    #[error("rate limit exceeded, retry after {retry_after} seconds")]
    RateLimitExceeded { retry_after: u64 },

    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let request_id = current_request_id();

        let (status, code, details) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found", None),
            AppError::CodeTaken => (StatusCode::CONFLICT, "code_taken", None),
            AppError::VersionConflict => (StatusCode::CONFLICT, "version_conflict", None),
            AppError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable", None),
            AppError::Validation {
                message: _,
                details,
            } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                Some(details.clone()),
            ),
            AppError::InvalidBody { status, .. } => (
                *status,
                match *status {
                    StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
                    StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
                    StatusCode::UNPROCESSABLE_ENTITY => "validation_error",
                    _ => "bad_request",
                },
                None,
            ),
            AppError::RateLimitExceeded { retry_after } => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(axum::http::header::RETRY_AFTER, retry_after.to_string())],
                    Json(ErrorBody {
                        code: "rate_limit_exceeded".to_string(),
                        message: format!("rate limit exceeded, retry after {retry_after} seconds"),
                        request_id,
                        details: None,
                    }),
                )
                    .into_response();
            }
            AppError::Internal(err) => {
                tracing::error!(error = ?err, request_id = ?request_id, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", None)
            }
        };

        let body = ErrorBody {
            code: code.to_string(),
            message: self.to_string(),
            request_id,
            details,
        };
        (status, Json(body)).into_response()
    }
}

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
