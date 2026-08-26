//! Хранилища для сервиса коротких ссылок.

mod cache;
mod in_memory;
mod postgres;
pub mod rate_limit_storage;
pub mod telemetry;

pub use cache::{Cache, CacheConfig, CacheStats, link_key};
pub use in_memory::InMemoryRepo;
pub use postgres::PostgresRepo;
pub use telemetry::spawn_pool_metrics;
