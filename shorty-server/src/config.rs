//! Конфигурация приложения.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub code_length: usize,
    pub max_generate_attempts: usize,
    pub request_timeout: Duration,
    pub max_body_bytes: usize,
    pub rate_limit_capacity: u64,
    pub rate_limit_period_secs: u64,
    pub rate_limit_cleanup_ttl_secs: u64,

    // Новые поля
    pub storage_type: StorageType,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub cache_ttl_secs: u64,
    pub cache_jitter_secs: u64,
    pub cache_op_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageType {
    InMemory,
    Postgres,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            code_length: 8,
            max_generate_attempts: 5,
            request_timeout: Duration::from_secs(5),
            max_body_bytes: 16 * 1024,
            rate_limit_capacity: 10,
            rate_limit_period_secs: 60,
            rate_limit_cleanup_ttl_secs: 120,
            storage_type: StorageType::InMemory,
            database_url: "postgres://postgres:postgres@localhost:5499/shorty".to_string(),
            redis_url: Some("redis://localhost:6379".to_string()),
            cache_ttl_secs: 60,
            cache_jitter_secs: 10,
            cache_op_timeout_ms: 300,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        // Основные настройки
        if let Ok(val) = std::env::var("CODE_LENGTH") {
            config.code_length = val.parse().expect("CODE_LENGTH must be a number");
        }

        if let Ok(val) = std::env::var("MAX_GENERATE_ATTEMPTS") {
            config.max_generate_attempts =
                val.parse().expect("MAX_GENERATE_ATTEMPTS must be a number");
        }

        if let Ok(val) = std::env::var("REQUEST_TIMEOUT_SECS") {
            config.request_timeout =
                Duration::from_secs(val.parse().expect("REQUEST_TIMEOUT_SECS must be a number"));
        }

        if let Ok(val) = std::env::var("MAX_BODY_BYTES") {
            config.max_body_bytes = val.parse().expect("MAX_BODY_BYTES must be a number");
        }

        if let Ok(val) = std::env::var("RATE_LIMIT_CAPACITY") {
            config.rate_limit_capacity = val.parse().expect("RATE_LIMIT_CAPACITY must be a number");
        }

        if let Ok(val) = std::env::var("RATE_LIMIT_PERIOD_SECS") {
            config.rate_limit_period_secs = val
                .parse()
                .expect("RATE_LIMIT_PERIOD_SECS must be a number");
        }

        if let Ok(val) = std::env::var("RATE_LIMIT_CLEANUP_TTL_SECS") {
            config.rate_limit_cleanup_ttl_secs = val
                .parse()
                .expect("RATE_LIMIT_CLEANUP_TTL_SECS must be a number");
        }

        // Тип хранилища
        if let Ok(val) = std::env::var("STORAGE_TYPE") {
            config.storage_type = match val.to_lowercase().as_str() {
                "postgres" => StorageType::Postgres,
                "inmemory" | "in-memory" => StorageType::InMemory,
                _ => StorageType::InMemory,
            };
        }

        // PostgreSQL
        if let Ok(val) = std::env::var("DATABASE_URL") {
            config.database_url = val;
        }

        // Redis
        if let Ok(val) = std::env::var("REDIS_URL") {
            config.redis_url = Some(val);
        } else if std::env::var("DISABLE_REDIS").is_ok() {
            config.redis_url = None;
        }

        // Кеш
        if let Ok(val) = std::env::var("CACHE_TTL_SECS") {
            config.cache_ttl_secs = val.parse().expect("CACHE_TTL_SECS must be a number");
        }

        if let Ok(val) = std::env::var("CACHE_JITTER_SECS") {
            config.cache_jitter_secs = val.parse().expect("CACHE_JITTER_SECS must be a number");
        }

        if let Ok(val) = std::env::var("CACHE_OP_TIMEOUT_MS") {
            config.cache_op_timeout_ms = val.parse().expect("CACHE_OP_TIMEOUT_MS must be a number");
        }

        config
    }

    pub fn is_cache_enabled(&self) -> bool {
        self.redis_url.is_some()
    }
}
