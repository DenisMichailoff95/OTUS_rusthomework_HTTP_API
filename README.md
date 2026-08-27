# Shorty — Production-ready backend сокращения ссылок

Production-приближенный сервис сокращения URL с REST API, gRPC, PostgreSQL, Redis-кешем, JWT-аутентификацией и observability.

## Архитектурные решения

### Транспортный слой

Сервис exposes два протокола в одном процессе:
- **HTTP** (axum) — REST API на `/api/v1/*` + публичные эндпоинты (`/healthz`, `/readyz`, `/version`, `/auth/login`, `/{code}` для редиректа)
- **gRPC** (tonic) — порт 50051, reflection + health checking

Доменный слой (`crates/domain`) и хранилище (`crates/storage`) полностью разделены от транспорта. Оба протокола используют те же `LinkRepository`, `ShortLink`, `LinkStats` — бизнес-логика не продублирована.

### Protobuf-контракт

Контракт лежит в `proto/shorty/v1/shorty.proto` (пакет `shorty.v1`). Описаны 4 метода:
- `GetLink` — получение по коду
- `CreateLink` — создание
- `ListLinks` — список с cursor-пагинацией (`page_size` + `page_token`)
- `StreamLinks` — server-streaming подписка на события

Все временные поля используют `google.protobuf.Timestamp`. Enum `LinkEventType` имеет нулевое значение `LINK_EVENT_TYPE_UNSPECIFIED`.

### Аутентификация и авторизация

- **JWT** подпись RSA (`RS256`) через асимметричные ключи. Поддерживается `kid` в заголовке (заготовка под ротацию ключей).
- **HTTP**: middleware проверяет `Authorization: Bearer <token>` на всех защищённых routes (`/api/v1/*`). Публичные routes: health, login, swagger-ui, openapi.json, редирект.
- **gRPC**: интерцептор извлекает Bearer-токен из metadata `authorization` и валидирует той же функцией, что и HTTP.
- Валидация: фиксированный алгоритм, `exp` обязателен, проверяются `iss` и `aud`. Ошибки → единый формат `ErrorBody` без раскрытия деталей.

### OpenAPI и Swagger UI

- Все handlers помечены `#[utoipa::path(...)]` с полным описанием параметров, тел и статусов ответов.
- DTO deriv `ToSchema` с примерами через `#[schema(example = ...)]`.
- Спецификация доступна по `/api-docs/openapi.json`.
- Swagger UI по `/swagger-ui` с поддержкой Authorize (Bearer JWT).

## Быстрый старт

```bash
# 1. Клонировать репозиторий
git clone <repository-url>
cd OTUS_rusthomework_HTTP_API

# 2. Сгенерировать JWT-ключи (если ещё не сгенерированы)
chmod +x scripts/gen-keys.sh
./scripts/gen-keys.sh

# 3. Поднять инфраструктуру
docker compose up -d

# 4. Собрать проект
cargo build --workspace

# 5. Запустить сервис (HTTP на 8080, gRPC на 50051)
cargo run --package shorty-server
```

## Переменные окружения

```env
# Хранилище
STORAGE_TYPE=postgres
DATABASE_URL=postgres://postgres:postgres@localhost:5499/shorty
REDIS_URL=redis://localhost:6379

# Сеть
LISTEN_ADDR=0.0.0.0:8080
GRPC_LISTEN_ADDR=0.0.0.0:50051

# Аутентификация (опционально, по умолчанию включена)
DISABLE_AUTH=1                    # отключить JWT
AUTH_ISSUER=shorty-service
AUTH_AUDIENCE=shorty-api
AUTH_TTL_SECS=900                 # 15 минут
AUTH_PRIVATE_KEY_PATH=./keys/jwt_private.pem
AUTH_PUBLIC_KEY_PATH=./keys/jwt_public.pem

# Кеш
CACHE_TTL_SECS=60
CACHE_JITTER_SECS=10
CACHE_OP_TIMEOUT_MS=300

# Логирование
RUST_LOG=info,sqlx=warn
LOG_FORMAT=json
```

## Получение токена

```bash
curl -X POST http://localhost:8080/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin"}'
```

Ответ:
```json
{
  "access_token": "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9...",
  "token_type": "Bearer",
  "expires_in": 900
}
```

## REST API

Все защищённые эндпоинты требуют заголовок `Authorization: Bearer <token>`.

### Создание ссылки

```bash
curl -X POST http://localhost:8080/api/v1/links \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"target_url":"https://example.com","custom_code":"promo2026","ttl_seconds":3600}'
```

### Получение ссылки

```bash
curl http://localhost:8080/api/v1/links/promo2026 \
  -H "Authorization: Bearer $TOKEN"
```

### Обновление ссылки (optimistic locking)

```bash
curl -X PUT http://localhost:8080/api/v1/links/promo2026 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"target_url":"https://example.com/updated","version":1}'
```

### Удаление ссылки

```bash
curl -X DELETE http://localhost:8080/api/v1/links/promo2026 \
  -H "Authorization: Bearer $TOKEN"
```

### Листинг ссылок (cursor-пагинация)

```bash
curl "http://localhost:8080/api/v1/links?limit=20" \
  -H "Authorization: Bearer $TOKEN"

curl "http://localhost:8080/api/v1/links?limit=20&cursor=<next_cursor>" \
  -H "Authorization: Bearer $TOKEN"
```

### Редирект (публичный)

```bash
curl -I http://localhost:8080/promo2026
```

### Метрики

```bash
curl http://localhost:8080/metrics
```

### Health и Version (публичные)

```bash
curl http://localhost:8080/healthz
curl http://localhost:8080/version
```

## gRPC API

Сервер слушает на `0.0.0.0:50051`.

### Примеры вызовов через grpcurl

```bash
# Получить ссылку по коду
grpcurl -plaintext -d '{"code":"promo2026"}' \
  localhost:50051 shorty.v1.ShortyService/GetLink

# Создать ссылку
grpcurl -plaintext -d '{"target_url":"https://example.com","custom_code":"grpc-demo","ttl_seconds":3600}' \
  localhost:50051 shorty.v1.ShortyService/CreateLink

# Список ссылок
grpcurl -plaintext -d '{"page_size":10}' \
  localhost:50051 shorty.v1.ShortyService/ListLinks

# Server-streaming подписка
grpcurl -plaintext -d '{"batch_size":5}' \
  localhost:50051 shorty.v1.ShortyService/StreamLinks
```

### Аутентификация в gRPC

```bash
# С токеном
grpcurl -H "authorization: Bearer $TOKEN" \
  -plaintext -d '{"code":"promo2026"}' \
  localhost:50051 shorty.v1.ShortyService/GetLink

# Без токена (вернёт Unauthenticated)
grpcurl -plaintext -d '{"code":"promo2026"}' \
  localhost:50051 shorty.v1.ShortyService/GetLink
```

### Reflection

Сервис исследуем через `grpcui` или `grpcurl` без локальных `.proto` файлов:

```bash
grpcurl -plaintext localhost:50051 list
grpcui -plaintext localhost:50051
```

## Swagger UI

Открыть: http://localhost:8080/swagger-ui

1. Нажать кнопку **Authorize**
2. Вставить токен в формате `Bearer <access_token>`
3. Выполнять запросы напрямую из интерфейса

## Генерация ключей

```bash
# Сгенерировать ES256 ключи (рекомендуется)
openssl ecparam -genkey -name prime256v1 -out keys/jwt_private.pem
openssl ec -in keys/jwt_private.pem -pubout -out keys/jwt_public.pem

# Или RSA ключи
openssl genpkey -algorithm RSA -out keys/jwt_private.pem -pkeyopt rsa_keygen_bits:2048
openssl rsa -in keys/jwt_private.pem -pubout -out keys/jwt_public.pem
```

> Приватные ключи не закоммичены в репозиторий (см. `.gitignore`). Для локального запуска используйте `scripts/gen-keys.sh`.

## Тестирование

```bash
# Unit и HTTP-тесты (без инфраструктуры)
cargo test --workspace

# Security-тесты (негативные сценарии JWT)
cargo test --test security

# Snapshot-тест OpenAPI (обновить: UPDATE_SNAPSHOT=1)
cargo test --test openapi_snapshot -- --ignored
UPDATE_SNAPSHOT=1 cargo test --test openapi_snapshot -- --ignored

# Интеграционные тесты gRPC (требует запущенный сервер)
cargo run --package shorty-server &
cargo test --test grpc_integration -- --ignored

# Интеграционные тесты PostgreSQL (требует docker compose up -d)
cargo test --test postgres_integration -- --ignored
```

### Security-тесты

- Без токена → 401
- С истёкшим токеном → 401
- С неверной подписью → 401
- С неверным `aud` → 401
- Валидный токен → 200

### gRPC-тесты

- Успешный вызов с токеном
- Unauthenticated без токена
- NotFound для несуществующего ID

## Сборка и проверки

```bash
# Форматирование
cargo fmt -- --check

# Lint
cargo clippy --workspace -- -D warnings

# Сборка (требует .sqlx/ в репозитории)
SQLX_OFFLINE=true cargo build --workspace
```

## Миграции

Миграции применяются автоматически при старте сервиса через `sqlx::migrate!`. Каталог `crates/storage/migrations/`.

## Observability

- **Tracing**: `tracing` + `tracing-subscriber` с `EnvFilter` (`RUST_LOG`). Поддержка JSON-формата (`LOG_FORMAT=json`). gRPC-вызовы прокидывают request/correlation ID в tracing-span.
- **Request ID**: `x-request-id` генерируется или принимается из запроса, добавляется в корневой span и заголовок ответа.
- **Metrics**: Prometheus exporter на `/metrics`.

## Graceful Shutdown

Сервис корректно завершает работу по `Ctrl+C`/`SIGTERM`:
1. При получении сигнала сразу помечает себя неготовым (`/readyz` → 503)
2. Останавливает приём новых запросов
3. Дождится завершения in-flight запросов (до 10 секунд)
4. Останавливает фоновые задачи (cleaner)
5. Закрывает соединения с БД и Redis

## Operations

### Переменные окружения

| Переменная | Назначение | Default |
|------------|-----------|---------|
| `STORAGE_TYPE` | Тип хранилища: `postgres` или `inmemory` | `postgres` |
| `DATABASE_URL` | Строка подключения PostgreSQL | `postgres://postgres:postgres@localhost:5499/shorty` |
| `REDIS_URL` | Строка подключения Redis | `redis://localhost:6379` |
| `LISTEN_ADDR` | Адрес HTTP-сервера | `0.0.0.0:8080` |
| `GRPC_LISTEN_ADDR` | Адрес gRPC-сервера | `0.0.0.0:50051` |
| `RUST_LOG` | Уровень логирования | `info` |
| `LOG_FORMAT` | Формат логов: `json` или `pretty` | `pretty` |
| `CACHE_TTL_SECS` | TTL кеша в секундах | `60` |
| `CACHE_JITTER_SECS` | Jitter TTL кеша | `10` |
| `CACHE_OP_TIMEOUT_MS` | Таймаут операций кеша | `300` |
| `RATE_LIMIT_CAPACITY` | Ёмкость rate limiter | `10` |
| `RATE_LIMIT_PERIOD_SECS` | Период rate limiter | `60` |
| `DISABLE_AUTH` | Отключить JWT (только для dev) | — |
| `AUTH_ISSUER` | Issuer JWT | `shorty-service` |
| `AUTH_AUDIENCE` | Audience JWT | `shorty-api` |
| `AUTH_TTL_SECS` | Время жизни access-токена | `900` |
| `AUTH_PRIVATE_KEY_PATH` | Путь к приватному ключу | `./keys/jwt_private.pem` |
| `AUTH_PUBLIC_KEY_PATH` | Путь к публичному ключу | `./keys/jwt_public.pem` |

### Health checks

- **`GET /healthz`** — liveness probe. Тривиальный 200, не зависит от внешних зависимостей.
- **`GET /readyz`** — readiness probe. Проверяет доступность PostgreSQL (`SELECT 1` с таймаутом 2с) и возвращает 503 во время shutdown.

### Формат логов

Структурированный JSON в stdout:
```json
{
  "timestamp": "2026-08-27T10:00:00Z",
  "level": "INFO",
  "message": "HTTP request processed",
  "method": "GET",
  "uri": "/api/v1/links",
  "status": 200,
  "latency_ms": 12,
  "request_id": "abc-123"
}
```

Управляется через `RUST_LOG` и `LOG_FORMAT=json`.

### Метрики Prometheus (`/metrics`)

- `http_requests_total{method, route, status}` — счётчик HTTP-запросов
- `http_request_duration_seconds{method, route}` — гистограмма latency
- `cache_hits_total` / `cache_misses_total` — попадания/промахи кеша
- `db_pool_connections` / `db_pool_idle_connections` / `db_pool_utilization` — метрики пула БД

### Поведение при отказе зависимостей

- **Redis недоступен**: сервис продолжает отвечать из PostgreSQL с `warn`-логом. Кеш отключён graceful degradation.
- **PostgreSQL недоступен**: `/readyz` возвращает 503, новые запросы получают 503, in-flight запросы завершаются с ошибкой.

### Получение JWT для проверки

```bash
curl -X POST http://localhost:8080/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin"}'
```

### Нагрузочная проверка

```bash
# Установите oha
cargo install oha

# 30 секунд, 10 параллельных соединений
oha -z 30s -c 10 http://localhost:8080/api/v1/links
```

Ожидаемые цифры на типичной машине (4 CPU, 8GB RAM):
- Throughput: ~2000–5000 req/s (read-heavy, с кешем)
- p50: <5ms, p99: <20ms

### Архитектура модулей

```
shorty-server/
  src/
    main.rs          — точка входа, graceful shutdown
    lib.rs           — AppState, роутер, middleware
    config.rs        — конфигурация из env
    auth/            — JWT: генерация, валидация, middleware
    api/
      handlers.rs    — HTTP handlers
      dto.rs         — Request/Response DTO + валидация
      error.rs       — единый ErrorBody, маппинг ошибок
      rate_limit.rs  — rate limiting middleware
      stats.rs       — handlers статистики
    grpc/
      mod.rs         — gRPC сервер, reflection, health, interceptor
    cleanup.rs       — фоновый cleaner просроченных ссылок
    request_id.rs    — x-request-id middleware
crates/
  domain/            — доменная логика, LinkRepository trait
  storage/           — PostgreSQL + Redis репозитории, миграции
proto/
  shorty/v1/         — protobuf-контракт
```

## Сборка Docker-образа

```bash
# Сборка образа
docker build -t shorty-server:latest .

# Запуск через docker-compose
docker compose up --build

# Проверка readiness
curl http://localhost:8080/readyz
```
