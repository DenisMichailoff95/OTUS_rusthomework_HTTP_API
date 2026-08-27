//! Аутентификация и авторизация.

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};
use utoipa::ToSchema;

use crate::{AppState, api::error::ErrorBody};

/// Ошибки аутентификации
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,

    #[error("missing token")]
    MissingToken,

    #[error("invalid token")]
    InvalidToken,

    #[error("token expired")]
    TokenExpired,

    #[error("forbidden")]
    Forbidden,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = match self {
            AuthError::InvalidCredentials | AuthError::InvalidToken => StatusCode::UNAUTHORIZED,
            AuthError::TokenExpired => StatusCode::UNAUTHORIZED,
            AuthError::MissingToken => StatusCode::UNAUTHORIZED,
            AuthError::Forbidden => StatusCode::FORBIDDEN,
        };
        let body = Json(serde_json::json!({
            "code": "auth_error",
            "message": self.to_string(),
            "request_id": crate::request_id::current_request_id(),
        }));
        (status, body).into_response()
    }
}

/// Claims JWT токена
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
    pub iss: String,
    pub aud: String,
    pub role: String,
}

impl Claims {
    pub fn new(sub: String, role: String, iss: String, aud: String, ttl_secs: i64) -> Self {
        let now = Utc::now();
        Self {
            sub,
            exp: (now + Duration::seconds(ttl_secs)).timestamp() as usize,
            iat: now.timestamp() as usize,
            iss,
            aud,
            role,
        }
    }
}

/// Аутентифицированный пользователь
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub claims: Claims,
}

/// Извлечение AuthUser из запроса
impl<S> axum::extract::FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|h| h.to_str().ok())
            .ok_or(AuthError::MissingToken)?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AuthError::InvalidToken)?;

        let (_, decoding_key) = load_keys(&AuthConfig::default())?;
        let claims = validate_token(token, &decoding_key)?;

        Ok(Self { claims })
    }
}

/// Конфигурация аутентификации
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub issuer: String,
    pub audience: String,
    pub access_token_ttl_secs: i64,
    pub private_key_path: String,
    pub public_key_path: String,
    pub admin_username: String,
    pub admin_password_hash: String,
    pub algorithm: Algorithm,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            issuer: "shorty-service".to_string(),
            audience: "shorty-api".to_string(),
            access_token_ttl_secs: 900, // 15 минут
            private_key_path: "../keys/jwt_private.pem".to_string(),
            public_key_path: "../keys/jwt_public.pem".to_string(),
            admin_username: "admin".to_string(),
            admin_password_hash: argon2_hash_password("admin"),
            algorithm: Algorithm::RS256,
        }
    }
}

/// Захешировать пароль через argon2
pub fn argon2_hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .expect("failed to hash password")
        .to_string()
}

/// Проверить пароль против argon2-хеша
pub fn verify_password(password: &str, hash: &str) -> Result<(), AuthError> {
    let parsed_hash = PasswordHash::new(hash).map_err(|_| AuthError::InvalidCredentials)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| AuthError::InvalidCredentials)
}

/// Resolve a path relative to CARGO_MANIFEST_DIR if it's not absolute.
fn resolve_path(path: &str, manifest_dir: &str) -> String {
    if path.starts_with('/') || path.starts_with("~/") || std::path::Path::new(path).is_absolute() {
        path.to_string()
    } else {
        std::path::PathBuf::from(manifest_dir)
            .join(path)
            .to_string_lossy()
            .into_owned()
    }
}

/// Загрузить RSA/EC ключи из файлов
pub fn load_keys(config: &AuthConfig) -> Result<(EncodingKey, DecodingKey), AuthError> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let private_pem = std::fs::read_to_string(resolve_path(&config.private_key_path, manifest_dir))
        .map_err(|e| {
            warn!(path = %config.private_key_path, error = ?e, "failed to read private key");
            AuthError::InvalidToken
        })?;
    let public_pem = std::fs::read_to_string(resolve_path(&config.public_key_path, manifest_dir))
        .map_err(|e| {
        warn!(path = %config.public_key_path, error = ?e, "failed to read public key");
        AuthError::InvalidToken
    })?;

    // Пробуем загрузить как RSA или EC в зависимости от алгоритма
    let encoding_key = match config.algorithm {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => {
            EncodingKey::from_rsa_pem(private_pem.as_bytes())
        }
        Algorithm::ES256 | Algorithm::ES384 => EncodingKey::from_ec_pem(private_pem.as_bytes()),
        _ => return Err(AuthError::InvalidToken),
    }
    .map_err(|e| {
        warn!(error = ?e, "failed to parse private key");
        AuthError::InvalidToken
    })?;

    let decoding_key = match config.algorithm {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => {
            DecodingKey::from_rsa_pem(public_pem.as_bytes())
        }
        Algorithm::ES256 | Algorithm::ES384 => DecodingKey::from_ec_pem(public_pem.as_bytes()),
        _ => return Err(AuthError::InvalidToken),
    }
    .map_err(|e| {
        warn!(error = ?e, "failed to parse public key");
        AuthError::InvalidToken
    })?;

    Ok((encoding_key, decoding_key))
}

/// Создать JWT токен
pub fn create_token(claims: &Claims, encoding_key: EncodingKey) -> Result<String, AuthError> {
    let algorithm = AuthConfig::default().algorithm;
    encode::<Claims>(&Header::new(algorithm), claims, &encoding_key).map_err(|e| {
        warn!(error = ?e, "failed to create token");
        AuthError::InvalidToken
    })
}

/// Валидировать JWT токен
pub fn validate_token(token: &str, public_key: &DecodingKey) -> Result<Claims, AuthError> {
    let config = AuthConfig::default();
    let mut validation = Validation::new(config.algorithm);
    validation.validate_exp = true;
    validation.leeway = 30;
    validation.set_issuer(&[&config.issuer]);
    validation.set_audience(&[&config.audience]);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);

    let token_data = decode::<Claims>(token, public_key, &validation).map_err(|e| {
        debug!(error = ?e, "token validation failed");
        match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
            _ => AuthError::InvalidToken,
        }
    })?;

    Ok(token_data.claims)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Имя пользователя
    #[schema(example = "admin")]
    pub username: String,
    /// Пароль
    #[schema(example = "admin")]
    pub password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    /// JWT токен
    pub access_token: String,
    /// Тип токена
    pub token_type: String,
    /// Время жизни в секундах
    pub expires_in: i64,
}

/// Обработчик входа
#[utoipa::path(
    post,
    path = "/auth/login",
    tags = ["auth"],
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Успешная аутентификация", body = LoginResponse),
        (status = 401, description = "Неверные учётные данные", body = ErrorBody),
        (status = 429, description = "Слишком много попыток", body = ErrorBody),
    )
)]
pub async fn login_handler(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    use crate::auth::verify_password;

    let config = state.config.auth.as_ref().unwrap();

    if req.username != config.admin_username {
        warn!(username = %req.username, "login failed: unknown user");
        return Err(AuthError::InvalidCredentials);
    }

    verify_password(&req.password, &config.admin_password_hash)?;

    let claims = Claims::new(
        req.username.clone(),
        "admin".to_string(),
        config.issuer.clone(),
        config.audience.clone(),
        config.access_token_ttl_secs,
    );

    let (encoding_key, _) = load_keys(config)?;
    let token = create_token(&claims, encoding_key)?;

    debug!(username = %req.username, "login successful");

    Ok(Json(LoginResponse {
        access_token: token,
        token_type: "Bearer".to_string(),
        expires_in: config.access_token_ttl_secs,
    }))
}

/// Middleware для проверки JWT
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, AuthError> {
    if state.auth.is_none() {
        return Ok(next.run(request).await);
    }

    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(AuthError::MissingToken)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AuthError::InvalidToken)?;

    let auth_config = state.auth.as_ref().unwrap();

    let (_, decoding_key) = load_keys(auth_config)?;
    let claims = validate_token(token, &decoding_key)?;

    let mut request = request;
    request.extensions_mut().insert(AuthUser { claims });

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_argon2_hash_and_verify() {
        let hash = argon2_hash_password("test_password");
        assert!(verify_password("test_password", &hash).is_ok());
        assert!(verify_password("wrong_password", &hash).is_err());
    }

    #[test]
    fn test_claims_creation() {
        let claims = Claims::new(
            "user123".to_string(),
            "user".to_string(),
            "issuer".to_string(),
            "audience".to_string(),
            300,
        );
        assert_eq!(claims.sub, "user123");
        assert_eq!(claims.role, "user");
    }
}
