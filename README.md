# Shorty — Сервис сокращения ссылок (ДЗ 2)

Production-приближенный HTTP-сервис сокращения URL с PostgreSQL, Redis-кешем и observability.

## Быстрый старт

```bash
# 1. Клонировать репозиторий
git clone <repository-url>
cd OTUS_rusthomework_HTTP_API

# 2. Поднять инфраструктуру
docker compose up -d

# 3. Собрать проект (offline-режим, .sqlx/ закоммичен)
SQLX_OFFLINE=true cargo build --workspace

# 4. Запустить сервис
STORAGE_TYPE=postgres DATABASE_URL=postgres://postgres:postgres@localhost:5499/shorty REDIS_URL=redis://localhost:6379 cargo run --package shorty-server
```

### Переменные окружения

См. `.env.example`:

```env
STORAGE_TYPE=postgres
DATABASE_URL=postgres://postgres:postgres@localhost:5499/shorty
REDIS_URL=redis://localhost:6379
LISTEN_ADDR=0.0.0.0:8080
LOG_FORMAT=json
CACHE_TTL_SECS=60
CACHE_JITTER_SECS=10
CACHE_OP_TIMEOUT_MS=300
RUST_LOG=info,sqlx=warn
```

## API

### Создание ссылки

```bash
curl -X POST http://localhost:8080/api/v1/links \
  -H "Content-Type: application/json" \
  -d '{"target_url":"https://example.com","custom_code":"promo2026","ttl_seconds":3600}'
```

### Получение ссылки

```bash
curl http://localhost:8080/api/v1/links/promo2026
```

### Обновление ссылки (optimistic locking)

```bash
curl -X PUT http://localhost:8080/api/v1/links/promo2026 \
  -H "Content-Type: application/json" \
  -d '{"target_url":"https://example.com/updated","version":1}'
```

### Удаление ссылки

```bash
curl -X DELETE http://localhost:8080/api/v1/links/promo2026
```

### Листинг ссылок (keyset pagination)

```bash
curl "http://localhost:8080/api/v1/links?limit=20"
curl "http://localhost:8080/api/v1/links?limit=20&cursor=<next_cursor>"
```

### Редирект

```bash
curl -I http://localhost:8080/promo2026
```

### Метрики

```bash
curl http://localhost:8080/metrics
```

## Схема ключей кеша

| Ключ | Описание |
|------|----------|
| `shorty:v1:link:{code}` | Данные ссылки в JSON с TTL 60с ± 10с jitter |

Сериализация: JSON. TTL: 60 секунд с jitter ±10 секунд для предотвращения массового истечения (thundering herd).

## Consistency

При успешном `PUT`/`DELETE` инвалидация кеша происходит **после коммита транзакции** в PostgreSQL. Порядок «сначала БД, потом кеш» исключает потерю актуальности данных.

Окно stale-данных ограничено TTL кеша (максимум 70 секунд при максимальном jitter). При остановке Redis сервис продолжает отвечать из PostgreSQL с warn-логом, что обеспечивает graceful degradation.

## Архитектурные решения

### Первичный ключ

Используется `UUID` (`gen_random_uuid()`) как PRIMARY KEY. Обоснование: распределённая генерация без координации, безопасность (неPredictable), совместимость с микросервисной архитектурой.

### Пагинация

Keyset (cursor) пагинация по `(created_at DESC, code DESC)`. Курсор — opaque base64-encoded JSON строка. Обоснование: стабильность при вставках между страницами, отсутствие проблем с пропуском/дублированием записей, что характерно для `OFFSET`.

### TTL кеша

TTTL 60 секунд с jitter ±10 секунд. Обоснование: достаточно для снижения нагрузки на БД при горячих чтениях, но ограничивает окно неактуальности данных. Jitter предотвращает simultaneous expiry (stampede).

### Индексы

- `idx_links_code` — уникальный поиск по коду (основной запрос `GET /links/{code}`)
- `idx_links_created_at_id` — keyset пагинация по дате создания
- `idx_links_expires_at` — очистка просроченных ссылок (`purge_expired`)

## Миграции

Миграции применяются автоматически при старте сервиса через `sqlx::migrate!`. Каталог `migrations/`.

```bash
# Применить миграции вручную
DATABASE_URL=postgres://postgres:postgres@localhost:5499/shorty cargo sqlx migrate run --source crates/storage/migrations
```

## Тестирование

```bash
# Unit и HTTP-тесты (без инфраструктуры)
SQLX_OFFLINE=true cargo test --workspace

# Интеграционные тесты с PostgreSQL (требует docker compose up -d)
cargo test --test postgres_integration -- --ignored
```

Интеграционные тесты покрывают:
- Happy-path CRUD
- 404 для несуществующей ссылки
- Конфликт optimistic locking (409)
- Корректность инвалидации кеша после `PUT`

## Observability

- **Tracing**: `tracing` + `tracing-subscriber` с `EnvFilter` (`RUST_LOG`). Поддержка JSON-формата (`LOG_FORMAT=json`).
- **Request ID**: `x-request-id` генерируется или принимается из запроса, добавляется в корневой span и заголовок ответа.
- **Metrics**: Prometheus exporter на `/metrics`. Счётчики `http_requests_total{method, route, status}`, гистограмма `http_request_duration_seconds{method, route}`, `cache_hits_total`, `cache_misses_total`, gauge пула БД.

## Сборка

```bash
# Проверка формата
cargo fmt -- --check

# Lint
cargo clippy --workspace -- -D warnings

# Сборка (требует .sqlx/ в репозитории)
SQLX_OFFLINE=true cargo build --workspace
```

## Graceful Shutdown

Сервис корректно завершает работу по `Ctrl+C`/`SIGTERM`, ожидая завершения фоновых задач (cleaner) в течение 10 секунд.
